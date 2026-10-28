// SPDX-License-Identifier: MPL-2.0
//! Own a private, unpublished context throughout consumer initialization.
use std::any::Any;
use surfman::{Context, Device, Error, Surface};

pub struct PendingContext {
    device: Option<Device>,
    context: Option<Context>,
    returned_surface: Option<Surface>,
    retired: bool,
    target_chain: Option<surfman::chains::SwapChain<Device>>,
    // Native display/window handles must outlive context destruction, including
    // an initialization rollback deferred while a surface is on loan.
    keep_alive: Vec<Box<dyn Any>>,
}
impl PendingContext {
    pub fn new(device: Device, context: Context) -> Self {
        Self {
            device: Some(device),
            context: Some(context),
            returned_surface: None,
            retired: false,
            target_chain: None,
            keep_alive: vec![],
        }
    }
    pub fn keep_alive(&mut self, owner: impl Any) {
        self.keep_alive.push(Box::new(owner));
    }
    pub fn parts(&mut self) -> (&Device, &mut Context) {
        (
            self.device.as_ref().unwrap(),
            self.context.as_mut().unwrap(),
        )
    }
    pub fn parts_mut(&mut self) -> (&mut Device, &mut Context) {
        (
            self.device.as_mut().unwrap(),
            self.context.as_mut().unwrap(),
        )
    }
    pub fn adopt_chain(&mut self, chain: surfman::chains::SwapChain<Device>) {
        assert!(self.target_chain.is_none());
        self.target_chain = Some(chain);
    }
    pub fn take_chain(&mut self) -> Option<surfman::chains::SwapChain<Device>> {
        self.target_chain.take()
    }
    pub fn bind(&mut self, surface: Surface) -> Result<(), Error> {
        let (device, context) = self.parts();
        match device.bind_surface_to_context(context, surface) {
            Ok(()) => Ok(()),
            Err((error, surface)) => {
                self.returned_surface = Some(surface);
                Err(error)
            },
        }
    }
    pub fn commit(mut self) -> (Device, Context) {
        assert!(self.returned_surface.is_none() && self.target_chain.is_none());
        (self.device.take().unwrap(), self.context.take().unwrap())
    }
    fn rollback(&mut self) -> bool {
        let Some(context) = self.context.as_mut() else {
            return true;
        };
        let device = self.device.as_ref().unwrap();
        // This context has no GL share group. Native destruction releases its
        // partial GL allocations, after any target-chain consumer loans drain.
        if !self.retired {
            if device.mark_context_lost(context).is_err() {
                return false;
            }
            self.retired = true;
        }
        if let Some(surface) = self.returned_surface.as_mut() {
            if device.destroy_surface(context, surface).is_err() {
                return false;
            }
            self.returned_surface = None;
        }
        if let Some(chain) = self.target_chain.as_ref() {
            if chain.destroy(device, context).is_err() {
                return false;
            }
            self.target_chain = None;
        }
        if device.destroy_context(context).is_err() {
            return false;
        }
        self.context = None;
        true
    }
}
impl Drop for PendingContext {
    fn drop(&mut self) {
        if self.context.is_none() {
            return;
        }
        let mut owner = Self {
            device: self.device.take(),
            context: self.context.take(),
            returned_surface: self.returned_surface.take(),
            retired: self.retired,
            target_chain: self.target_chain.take(),
            keep_alive: std::mem::take(&mut self.keep_alive),
        };
        webxr_api::deferred_cleanup::retire(move || owner.rollback());
    }
}

#[cfg(all(test, target_os = "linux"))]
mod tests {
    use super::*;
    use euclid::default::Size2D;
    use std::{cell::Cell, rc::Rc};
    use surfman::{
        ContextAttributeFlags, ContextAttributes, GLVersion, SurfaceAccess, SurfaceType,
    };
    struct Host(Rc<Cell<bool>>);
    impl Drop for Host {
        fn drop(&mut self) {
            self.0.set(false);
        }
    }
    #[test]
    fn rollback_keeps_host_alive_until_a_borrowed_surface_returns() {
        use surfman::chains::{PreserveBuffer, SwapChain, SwapChainAPI};
        let scope = webxr_api::deferred_cleanup::CleanupScope::new();
        let alive = Rc::new(Cell::new(true));
        let mut owner = owner();
        owner.keep_alive(Host(alive.clone()));
        let target = surface(&mut owner);
        owner.bind(target).unwrap();
        let (device, context) = owner.parts();
        let chain = SwapChain::create_attached(device, context, SurfaceAccess::GPUOnly).unwrap();
        chain
            .swap_buffers(device, context, PreserveBuffer::No)
            .unwrap();
        let loan = chain.take_surface().unwrap();
        owner.adopt_chain(chain.clone());
        scope.run(|| drop(owner));
        assert!(alive.get() && scope.pending());
        chain.recycle_surface(loan);
        webxr_api::deferred_cleanup::poll();
        assert!(!alive.get() && !scope.pending());
    }
    #[test]
    fn retained_scope_keeps_unpublished_native_context_and_host_inert() {
        let scope = webxr_api::deferred_cleanup::CleanupScope::new();
        let alive = Rc::new(Cell::new(true));
        let mut owner = owner();
        owner.keep_alive(Host(alive.clone()));
        scope.retain_unfinished();
        scope.run(|| drop(owner));
        webxr_api::deferred_cleanup::poll();
        assert!(alive.get() && scope.pending());
    }
    fn owner() -> PendingContext {
        let connection = surfman::mesa_surfaceless::connection::Connection::new().unwrap();
        let adapter = if std::env::var_os("DOME_XR_RETIRE_SOFTWARE").is_some() {
            connection.create_software_adapter().unwrap()
        } else {
            connection.create_low_power_adapter().unwrap()
        };
        let device = Device::Alternate(connection.create_device(&adapter).unwrap());
        let descriptor = device
            .create_context_descriptor(&ContextAttributes {
                version: GLVersion::new(3, 3),
                flags: ContextAttributeFlags::ALPHA,
            })
            .unwrap();
        let context = device.create_context(&descriptor, None).unwrap();
        PendingContext::new(device, context)
    }
    fn surface(owner: &mut PendingContext) -> Surface {
        let (device, context) = owner.parts();
        device
            .create_surface(
                context,
                SurfaceAccess::GPUOnly,
                SurfaceType::Generic {
                    size: Size2D::new(8, 8),
                },
            )
            .unwrap()
    }
    #[test]
    fn initialization_rollback_retires_attached_native_surface() {
        let mut owner = owner();
        let surface = surface(&mut owner);
        owner.bind(surface).unwrap();
        assert!(owner.rollback());
        assert!(owner.context.is_none());
        assert!(owner.rollback());
    }
    #[test]
    fn failed_bind_keeps_returned_surface_until_native_rollback() {
        let mut owner = owner();
        let surface = surface(&mut owner);
        let (device, context) = owner.parts();
        device.mark_context_lost(context).unwrap();
        assert!(owner.bind(surface).is_err());
        assert!(owner.returned_surface.is_some());
        assert!(owner.rollback());
        assert!(owner.context.is_none() && owner.returned_surface.is_none());
    }
    #[test]
    fn successful_commit_transfers_the_native_owner_without_retiring_it() {
        let mut owner = owner();
        let surface = surface(&mut owner);
        owner.bind(surface).unwrap();
        let (device, mut context) = owner.commit();
        assert!(!device.context_is_lost(&context));
        assert!(device.context_surface_info(&context).unwrap().is_some());
        device.destroy_context(&mut context).unwrap();
    }
    #[test]
    fn rollback_retains_context_until_target_consumer_returns() {
        use surfman::chains::{PreserveBuffer, SwapChain, SwapChainAPI};
        let mut owner = owner();
        let surface = surface(&mut owner);
        owner.bind(surface).unwrap();
        let (device, context) = owner.parts();
        let chain = SwapChain::create_attached(device, context, SurfaceAccess::GPUOnly).unwrap();
        chain
            .swap_buffers(device, context, PreserveBuffer::No)
            .unwrap();
        let loan = chain.take_surface().unwrap();
        owner.adopt_chain(chain.clone());
        assert!(!owner.rollback());
        assert!(owner.context.is_some() && owner.target_chain.is_some());
        chain.recycle_surface(loan);
        assert!(owner.rollback());
        assert!(owner.context.is_none() && owner.target_chain.is_none());
    }
    #[test]
    fn commit_transfers_target_chain_for_normal_teardown() {
        use surfman::chains::SwapChain;
        let mut owner = owner();
        let surface = surface(&mut owner);
        owner.bind(surface).unwrap();
        let (device, context) = owner.parts();
        let chain = SwapChain::create_attached(device, context, SurfaceAccess::GPUOnly).unwrap();
        owner.adopt_chain(chain);
        let chain = owner.take_chain().unwrap();
        let (device, mut context) = owner.commit();
        assert!(!device.context_is_lost(&context));
        chain.destroy(&device, &mut context).unwrap();
        device.destroy_context(&mut context).unwrap();
    }
}
