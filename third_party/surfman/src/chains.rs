/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/. */

//! An implementation of thread-safe swap chains for the `surfman` surface manager.
//!
//! The role of a swap chain is to allow surfaces to be communicated between contexts,
//! often in different threads. Each swap chain has a *producer* context,
//! responsible for creating and destroying surfaces, and a number of *consumer* contexts,
//! (usually just one) which take surfaces from the swap chain, and return them for recycling.
//!
//! Each swap chain has a *back buffer*, that is the current surface that the producer context may draw to.
//! Each swap chain has a *front buffer*, that is the most recent surface the producer context finished drawing to.
//!
//! The producer may *swap* these buffers when it has finished drawing and has a surface ready to display.
//!
//! The consumer may *take* the front buffer, display it, then *recycle* it.
//!
//! Each producer context has one *attached* swap chain, whose back buffer is the current surface of the context.
//! The producer may change the attached swap chain, attaching a currently unattached swap chain,
//! and detaching the currently attached one.

#![allow(missing_docs)]

use crate::device::Device as DeviceAPI;
use crate::{ContextID, Error, SurfaceAccess, SurfaceInfo, SurfaceType};
use euclid::default::Size2D;
use fnv::{FnvHashMap, FnvHashSet};
use glow as gl;
use glow::Context as Gl;
use glow::HasContext;
use log::debug;
use std::collections::hash_map::Entry;
use std::fmt::Debug;
use std::hash::Hash;
use std::mem;
use std::sync::{Arc, Mutex, MutexGuard, RwLock, RwLockReadGuard, RwLockWriteGuard};

#[cfg(test)]
#[path = "chains_ownership_tests.rs"]
mod ownership_tests;

// The data stored for each swap chain.
struct SwapChainData<Device: DeviceAPI> {
    // The size of the back buffer
    size: Size2D<i32>,
    // The id of the producer context
    context_id: ContextID,
    // The surface access mode for the context.
    surface_access: SurfaceAccess,
    // The back buffer of the swap chain.
    back_buffer: BackBuffer<Device>,
    // Some if the producing context has finished drawing a new front buffer, ready to be displayed.
    pending_surface: Option<Device::Surface>,
    // All of the surfaces that have already been displayed, ready to be recycled.
    recycled_surfaces: Vec<Device::Surface>,
    // Failed retirement may have released only part of a surface. Such surfaces
    // must never be offered to consumers or reused as drawing buffers.
    retired_surfaces: Vec<Device::Surface>,
    // A detached clear can temporarily displace another chain's attachment.
    // Keep both owners until restoring that attachment succeeds.
    temporary_attachment: Option<TemporaryAttachment<Device>>,
    consumer_loans: usize,
    closing: bool,
}

struct TemporaryAttachment<Device: DeviceAPI> {
    previous: Option<Device::Surface>,
    ours: Option<Device::Surface>,
}

pub enum PreserveBuffer<'a> {
    Yes(&'a Gl),
    No,
}

enum BackBuffer<Device: DeviceAPI> {
    Attached,
    Detached(Device::Surface),
    UnboundAttached(Device::Surface),
    TakenAttached,
    TakenDetached,
    Retired,
}

impl<Device: DeviceAPI> BackBuffer<Device> {
    fn take_surface(
        &mut self,
        device: &Device,
        context: &mut Device::Context,
    ) -> Result<Device::Surface, Error> {
        let new_back_buffer = match self {
            BackBuffer::Attached | BackBuffer::UnboundAttached(_) => BackBuffer::TakenAttached,
            BackBuffer::Detached(_) => BackBuffer::TakenDetached,
            _ => return Err(Error::Failed),
        };
        let surface = match mem::replace(self, new_back_buffer) {
            BackBuffer::Attached => match device.unbind_surface_from_context(context) {
                Ok(Some(surface)) => surface,
                Ok(None) => {
                    *self = BackBuffer::Attached;
                    return Err(Error::Failed);
                },
                Err(error) => {
                    *self = BackBuffer::Attached;
                    return Err(error);
                },
            },
            BackBuffer::Detached(surface) | BackBuffer::UnboundAttached(surface) => surface,
            _ => unreachable!(),
        };
        Ok(surface)
    }
    fn take_surface_texture(
        &mut self,
        device: &Device,
        context: &mut Device::Context,
    ) -> Result<Device::SurfaceTexture, Error> {
        let surface = self.take_surface(device, context)?;
        match device.create_surface_texture(context, surface) {
            Ok(texture) => Ok(texture),
            Err((error, surface)) => {
                self.restore_surface(device, context, surface);
                Err(error)
            },
        }
    }
    fn replace_surface(
        &mut self,
        device: &Device,
        context: &mut Device::Context,
        surface: Device::Surface,
    ) -> Result<(), (Error, Device::Surface)> {
        let new_back_buffer = match self {
            BackBuffer::TakenAttached => {
                if let Err(error) = device.bind_surface_to_context(context, surface) {
                    return Err(error);
                }
                BackBuffer::Attached
            },
            BackBuffer::TakenDetached => BackBuffer::Detached(surface),
            _ => return Err((Error::Failed, surface)),
        };
        *self = new_back_buffer;
        Ok(())
    }
    // Called only after taking this slot. A failed rollback retains its role
    // and the original allocation, allowing the next operation to retry it.
    fn restore_surface(
        &mut self,
        device: &Device,
        context: &mut Device::Context,
        surface: Device::Surface,
    ) {
        if let Err((_, surface)) = self.replace_surface(device, context, surface) {
            // All callers hold a TakenAttached/Detached slot. Detached restores
            // cannot fail; attached restores may need another native bind.
            *self = BackBuffer::UnboundAttached(surface);
        }
    }

    fn ensure_attached(
        &mut self,
        device: &Device,
        context: &mut Device::Context,
    ) -> Result<(), Error> {
        if matches!(self, BackBuffer::UnboundAttached(_)) {
            let surface = self.take_surface(device, context)?;
            if let Err((error, surface)) = self.replace_surface(device, context, surface) {
                *self = BackBuffer::UnboundAttached(surface);
                return Err(error);
            }
        }
        if matches!(self, BackBuffer::Attached) {
            Ok(())
        } else {
            Err(Error::Failed)
        }
    }

    fn available(&self) -> bool {
        matches!(
            self,
            BackBuffer::Attached | BackBuffer::Detached(_) | BackBuffer::UnboundAttached(_)
        )
    }
}

impl<Device: DeviceAPI> SwapChainData<Device> {
    // Keep the previous buffer's role on failure and return the unused new
    // allocation to its caller. No cleanup operation may discard either one.
    fn replace_back_buffer(
        &mut self,
        device: &Device,
        context: &mut Device::Context,
        new: Device::Surface,
    ) -> Result<Device::Surface, (Error, Device::Surface)> {
        let old = match self.back_buffer.take_surface(device, context) {
            Ok(old) => old,
            Err(error) => return Err((error, new)),
        };
        if let Err((error, new)) = self.back_buffer.replace_surface(device, context, new) {
            self.back_buffer.restore_surface(device, context, old);
            return Err((error, new));
        }
        Ok(old)
    }

    fn restore_temporary_attachment(
        &mut self,
        device: &Device,
        context: &mut Device::Context,
    ) -> Result<(), Error> {
        let Some(pending) = self.temporary_attachment.as_mut() else {
            return Ok(());
        };
        if pending.ours.is_none() {
            pending.ours = Some(
                device
                    .unbind_surface_from_context(context)?
                    .ok_or(Error::Failed)?,
            );
        }
        if let Some(previous) = pending.previous.take() {
            if let Err((error, previous)) = device.bind_surface_to_context(context, previous) {
                pending.previous = Some(previous);
                return Err(error);
            }
        }
        // The detached slot remains TakenDetached until the entire temporary
        // attachment has been unwound. Restoring it performs no native bind.
        let pending = self.temporary_attachment.take().unwrap();
        self.back_buffer = BackBuffer::Detached(pending.ours.unwrap());
        Ok(())
    }

    fn recover_attachment(
        &mut self,
        device: &Device,
        context: &mut Device::Context,
    ) -> Result<(), Error> {
        self.validate_context(device, context)?;
        self.restore_temporary_attachment(device, context)?;
        if self.is_attached() {
            self.back_buffer.ensure_attached(device, context)
        } else if self.back_buffer.available() {
            Ok(())
        } else {
            Err(Error::Failed)
        }
    }

    // Returns `Ok` if `context` is the producer context for this swap chain.
    fn validate_context(&self, device: &Device, context: &Device::Context) -> Result<(), Error> {
        if self.context_id == device.context_id(context) {
            Ok(())
        } else {
            Err(Error::IncompatibleContext)
        }
    }

    // Swap the back and front buffers.
    // Called by the producer.
    // Returns an error if `context` is not the producer context for this swap chain.
    fn swap_buffers(
        &mut self,
        device: &Device,
        context: &mut Device::Context,
        preserve_buffer: PreserveBuffer<'_>,
    ) -> Result<(), Error> {
        debug!("Swap buffers on context {:?}", self.context_id);
        self.validate_context(device, context)?;
        if self.closing {
            return Err(Error::Failed);
        }
        self.restore_temporary_attachment(device, context)?;
        if !self.back_buffer.available() {
            return Err(Error::Failed);
        }

        // Fetch a new back buffer, recycling presented buffers if possible.
        let recycled = self
            .recycled_surfaces
            .iter()
            .position(|surface| device.surface_info(surface).size == self.size);
        let mut used_pending = false;
        let new_back_buffer = if let Some(index) = recycled {
            self.recycled_surfaces.swap_remove(index)
        } else if self
            .pending_surface
            .as_ref()
            .is_some_and(|surface| device.surface_info(surface).size == self.size)
        {
            used_pending = true;
            self.pending_surface.take().unwrap()
        } else {
            let surface_type = SurfaceType::Generic { size: self.size };
            device.create_surface(context, self.surface_access, surface_type)?
        };

        let back_info = device.surface_info(&new_back_buffer);

        // Swap the buffers
        debug!(
            "Surface {:?} is the new back buffer for context {:?}",
            device.surface_info(&new_back_buffer).id,
            self.context_id
        );
        let new_front_buffer = match self.replace_back_buffer(device, context, new_back_buffer) {
            Ok(surface) => surface,
            Err((error, surface)) => {
                if used_pending {
                    self.pending_surface = Some(surface);
                } else {
                    self.recycled_surfaces.push(surface);
                }
                return Err(error);
            },
        };

        if let PreserveBuffer::Yes(gl) = preserve_buffer {
            let front_info = device.surface_info(&new_front_buffer);
            unsafe {
                gl.bind_framebuffer(gl::READ_FRAMEBUFFER, front_info.framebuffer_object);
                debug_assert_eq!(gl.get_error(), gl::NO_ERROR);
                gl.bind_framebuffer(gl::DRAW_FRAMEBUFFER, back_info.framebuffer_object);
                debug_assert_eq!(gl.get_error(), gl::NO_ERROR);
                gl.blit_framebuffer(
                    0,
                    0,
                    front_info.size.width,
                    front_info.size.height,
                    0,
                    0,
                    back_info.size.width,
                    back_info.size.height,
                    gl::COLOR_BUFFER_BIT | gl::DEPTH_BUFFER_BIT | gl::STENCIL_BUFFER_BIT,
                    gl::NEAREST,
                );
                debug_assert_eq!(gl.get_error(), gl::NO_ERROR);
            }
        }

        // Update the state
        debug!(
            "Surface {:?} is the new front buffer for context {:?}",
            device.surface_info(&new_front_buffer).id,
            self.context_id
        );
        if let Some(old_front) = self.pending_surface.replace(new_front_buffer) {
            self.recycled_surfaces.push(old_front);
        }
        Ok(())
    }

    // Swap the attached swap chain.
    // Called by the producer.
    // Returns an error if `context` is not the producer context for both swap chains.
    // Returns an error if this swap chain is attached, or the other swap chain is detached.
    fn take_attachment_from(
        &mut self,
        device: &Device,
        context: &mut Device::Context,
        other: &mut SwapChainData<Device>,
    ) -> Result<(), Error> {
        self.validate_context(device, context)?;
        if self.closing {
            return Err(Error::Failed);
        }
        other.validate_context(device, context)?;
        self.restore_temporary_attachment(device, context)?;
        other.restore_temporary_attachment(device, context)?;
        if self.is_attached()
            || !other.is_attached()
            || !self.back_buffer.available()
            || !other.back_buffer.available()
        {
            return Err(Error::Failed);
        }
        let our_surface = self.back_buffer.take_surface(device, context)?;
        let their_surface = match other.back_buffer.take_surface(device, context) {
            Ok(surface) => surface,
            Err(error) => {
                self.back_buffer
                    .restore_surface(device, context, our_surface);
                return Err(error);
            },
        };
        mem::swap(&mut self.back_buffer, &mut other.back_buffer);
        if let Err((error, our_surface)) =
            self.back_buffer
                .replace_surface(device, context, our_surface)
        {
            mem::swap(&mut self.back_buffer, &mut other.back_buffer);
            self.back_buffer
                .restore_surface(device, context, our_surface);
            other
                .back_buffer
                .restore_surface(device, context, their_surface);
            return Err(error);
        }
        // The other chain is now detached; its replacement cannot fail.
        other
            .back_buffer
            .restore_surface(device, context, their_surface);
        Ok(())
    }

    // Resize the swap chain.
    // This creates a new back buffer of the appropriate size,
    // and destroys the old one.
    // Called by the producer.
    // Returns an error if `context` is not the producer context for this swap chain.
    // Returns an error if `size` is smaller than (1, 1).
    fn resize(
        &mut self,
        device: &Device,
        context: &mut Device::Context,
        size: Size2D<i32>,
    ) -> Result<(), Error> {
        debug!(
            "Resizing context {:?} to {:?}",
            device.context_id(context),
            size
        );
        self.validate_context(device, context)?;
        if self.closing {
            return Err(Error::Failed);
        }
        self.restore_temporary_attachment(device, context)?;
        if !self.back_buffer.available() {
            return Err(Error::Failed);
        }
        if (size.width < 1) || (size.height < 1) {
            return Err(Error::Failed);
        }
        let surface_type = SurfaceType::Generic { size };
        let new_back_buffer = device.create_surface(context, self.surface_access, surface_type)?;
        let old_back_buffer = match self.replace_back_buffer(device, context, new_back_buffer) {
            Ok(surface) => surface,
            Err((error, surface)) => {
                self.recycled_surfaces.push(surface);
                return Err(error);
            },
        };
        self.size = size;
        self.retired_surfaces.push(old_back_buffer);
        self.retired_surfaces.append(&mut self.recycled_surfaces);
        crate::retirement::retire_pending(&mut self.retired_surfaces, |surface| {
            device.destroy_surface(context, surface)
        })
    }

    // Get the current size.
    // Called by a consumer.
    fn size(&self) -> Size2D<i32> {
        self.size
    }

    // Take the current back buffer.
    // Called by a producer.
    fn take_surface_texture(
        &mut self,
        device: &Device,
        context: &mut Device::Context,
    ) -> Result<Device::SurfaceTexture, Error> {
        self.validate_context(device, context)?;
        if self.closing {
            return Err(Error::Failed);
        }
        self.restore_temporary_attachment(device, context)?;
        self.back_buffer.take_surface_texture(device, context)
    }

    // Recycle the current back buffer.
    // Called by a producer.
    fn recycle_surface_texture(
        &mut self,
        device: &Device,
        context: &mut Device::Context,
        surface_texture: &mut Option<Device::SurfaceTexture>,
    ) -> Result<(), Error> {
        self.validate_context(device, context)?;
        self.restore_temporary_attachment(device, context)?;
        if !matches!(
            self.back_buffer,
            BackBuffer::TakenAttached | BackBuffer::TakenDetached
        ) {
            return Err(Error::Failed);
        }
        let texture = surface_texture.take().ok_or(Error::Failed)?;
        let surface = match device.destroy_surface_texture(context, texture) {
            Ok(surface) => surface,
            Err((error, texture)) => {
                *surface_texture = Some(texture);
                return Err(error);
            },
        };
        if let Err((error, surface)) = self.back_buffer.replace_surface(device, context, surface) {
            self.back_buffer = BackBuffer::UnboundAttached(surface);
            return Err(error);
        }
        Ok(())
    }

    // Take the current front buffer.
    // Returns the most recent recycled surface if there is no current front buffer.
    // Called by a consumer.
    fn take_surface(&mut self) -> Option<Device::Surface> {
        if self.closing || matches!(self.back_buffer, BackBuffer::Retired) {
            return None;
        }
        let surface = self
            .pending_surface
            .take()
            .or_else(|| self.recycled_surfaces.pop());
        if surface.is_some() {
            self.consumer_loans += 1;
        }
        surface
    }

    // Take the current front buffer.
    // Returns `None` if there is no current front buffer.
    // Called by a consumer.
    fn take_pending_surface(&mut self) -> Option<Device::Surface> {
        if self.closing {
            return None;
        }
        let surface = self.pending_surface.take();
        if surface.is_some() {
            self.consumer_loans += 1;
        }
        surface
    }

    // Recycle the current front buffer.
    // Called by a consumer.
    fn recycle_surface(&mut self, surface: Device::Surface) {
        // Each consumer return balances one successful take, including imports
        // that failed and returned the original raw surface.
        assert!(
            self.consumer_loans > 0,
            "surface returned without a consumer loan"
        );
        self.consumer_loans -= 1;
        if self.closing || matches!(self.back_buffer, BackBuffer::Retired) {
            self.retired_surfaces.push(surface);
        } else {
            self.recycled_surfaces.push(surface);
        }
    }

    // Clear the current back buffer. The producer's GL context must be current.
    // On failure, retry this chain before using another attachment on that context.
    fn clear_surface(
        &mut self,
        device: &Device,
        context: &mut Device::Context,
        gl: &Gl,
        color: [f32; 4],
    ) -> Result<(), Error> {
        self.validate_context(device, context)?;
        if self.closing {
            return Err(Error::Failed);
        }
        self.restore_temporary_attachment(device, context)?;
        if !self.back_buffer.available() {
            return Err(Error::Failed);
        }
        if self.is_attached() {
            self.back_buffer.ensure_attached(device, context)?;
        }

        let draw_fbo;
        let read_fbo;
        let mut clear_color = [0.; 4];
        let mut clear_depth = [0.];
        let mut clear_stencil = [0];
        let color_mask;
        let depth_mask;
        let stencil_mask;
        let back_stencil_mask;
        let scissor_enabled = unsafe { gl.is_enabled(gl::SCISSOR_TEST) };
        let rasterizer_enabled = unsafe { gl.is_enabled(gl::RASTERIZER_DISCARD) };
        unsafe {
            draw_fbo = gl.get_parameter_framebuffer(gl::DRAW_FRAMEBUFFER_BINDING);
            read_fbo = gl.get_parameter_framebuffer(gl::READ_FRAMEBUFFER_BINDING);
            gl.get_parameter_f32_slice(gl::COLOR_CLEAR_VALUE, &mut clear_color);
            gl.get_parameter_f32_slice(gl::DEPTH_CLEAR_VALUE, &mut clear_depth);
            gl.get_parameter_i32_slice(gl::STENCIL_CLEAR_VALUE, &mut clear_stencil);
            depth_mask = gl.get_parameter_bool(gl::DEPTH_WRITEMASK);
            stencil_mask = gl
                .get_parameter_u32(gl::STENCIL_WRITEMASK)
                .ok_or(Error::Failed)?;
            back_stencil_mask = gl
                .get_parameter_u32(gl::STENCIL_BACK_WRITEMASK)
                .ok_or(Error::Failed)?;
            color_mask = gl.get_parameter_bool_array::<4>(gl::COLOR_WRITEMASK);
        }

        let operation = (|| {
            if !self.is_attached() {
                let ours = self.back_buffer.take_surface(device, context)?;
                let previous = match device.unbind_surface_from_context(context) {
                    Ok(previous) => previous,
                    Err(error) => {
                        self.back_buffer.restore_surface(device, context, ours);
                        return Err(error);
                    },
                };
                self.temporary_attachment = Some(TemporaryAttachment {
                    previous,
                    ours: None,
                });
                if let Err((error, ours)) = device.bind_surface_to_context(context, ours) {
                    self.temporary_attachment.as_mut().unwrap().ours = Some(ours);
                    return Err(error);
                }
            }
            let fbo = device
                .context_surface_info(context)?
                .ok_or(Error::Failed)?
                .framebuffer_object;
            unsafe {
                gl.bind_framebuffer(gl::FRAMEBUFFER, fbo);
                gl.clear_color(color[0], color[1], color[2], color[3]);
                gl.clear_depth(1.);
                gl.clear_stencil(0);
                gl.disable(gl::SCISSOR_TEST);
                gl.disable(gl::RASTERIZER_DISCARD);
                gl.depth_mask(true);
                gl.stencil_mask(u32::MAX);
                gl.color_mask(true, true, true, true);
                gl.clear(gl::COLOR_BUFFER_BIT | gl::DEPTH_BUFFER_BIT | gl::STENCIL_BUFFER_BIT);
            }
            Ok(())
        })();
        // Loss requires backend readiness, not further ordinary GL commands.
        let context_lost = |error: &Error| {
            matches!(
                error,
                Error::MakeCurrentFailed(crate::WindowingApiError::ContextLost)
                    | Error::PresentFailed(crate::WindowingApiError::ContextLost)
            )
        };
        if operation.as_ref().is_err_and(context_lost) {
            return operation;
        }
        let restored_attachment = self.restore_temporary_attachment(device, context);
        if !restored_attachment.as_ref().is_err_and(context_lost) {
            unsafe {
                gl.bind_framebuffer(gl::DRAW_FRAMEBUFFER, draw_fbo);
                gl.bind_framebuffer(gl::READ_FRAMEBUFFER, read_fbo);
                gl.clear_color(
                    clear_color[0],
                    clear_color[1],
                    clear_color[2],
                    clear_color[3],
                );
                gl.color_mask(color_mask[0], color_mask[1], color_mask[2], color_mask[3]);
                gl.clear_depth(clear_depth[0] as f64);
                gl.clear_stencil(clear_stencil[0]);
                gl.depth_mask(depth_mask);
                gl.stencil_mask_separate(gl::FRONT, stencil_mask);
                gl.stencil_mask_separate(gl::BACK, back_stencil_mask);
                if scissor_enabled {
                    gl.enable(gl::SCISSOR_TEST);
                } else {
                    gl.disable(gl::SCISSOR_TEST);
                }
                if rasterizer_enabled {
                    gl.enable(gl::RASTERIZER_DISCARD);
                } else {
                    gl.disable(gl::RASTERIZER_DISCARD);
                }
            }
        }
        operation.and(restored_attachment)
    }

    /// Is this the attached swap chain?
    fn is_attached(&self) -> bool {
        match self.back_buffer {
            BackBuffer::Attached | BackBuffer::UnboundAttached(_) | BackBuffer::TakenAttached => {
                true
            },
            BackBuffer::Detached(_) | BackBuffer::TakenDetached | BackBuffer::Retired => false,
        }
    }

    // Destroy the swap chain.
    // Called by the producer.
    // Returns an error if `context` is not the producer context for this swap chain.
    fn destroy(&mut self, device: &Device, context: &mut Device::Context) -> Result<(), Error> {
        self.validate_context(device, context)?;
        self.closing = true;
        self.restore_temporary_attachment(device, context)?;
        let mut error = None;
        if self.back_buffer.available() {
            match self.back_buffer.take_surface(device, context) {
                Ok(surface) => {
                    self.retired_surfaces.push(surface);
                    self.back_buffer = BackBuffer::Retired;
                },
                Err(failure) => error = Some(failure),
            }
        } else if !matches!(self.back_buffer, BackBuffer::Retired) {
            // A producer still owns the texture borrowed from this slot.
            error = Some(Error::Failed);
        }
        if let Some(surface) = self.pending_surface.take() {
            self.retired_surfaces.push(surface);
        }
        self.retired_surfaces.append(&mut self.recycled_surfaces);
        let retired = crate::retirement::retire_pending(&mut self.retired_surfaces, |surface| {
            device.destroy_surface(context, surface)
        });
        // Native context ownership must outlive every surface in a consumer.
        // Keep the registry entry so the producer can retry after returns.
        if self.consumer_loans != 0 {
            return Err(error.unwrap_or(Error::Failed));
        }
        match error {
            Some(error) => Err(error),
            None => retired,
        }
    }
}

/// A consumer-local imported surface. Keep this owner on the consuming thread
/// until `release` succeeds; its texture may be neither Send nor Sync.
pub struct SurfaceConsumer<Device: DeviceAPI> {
    chain: SwapChain<Device>,
    texture: Option<Device::SurfaceTexture>,
    size: Size2D<i32>,
}
impl<Device: DeviceAPI> SurfaceConsumer<Device> {
    /// Import a front buffer, returning the raw loan on every import failure.
    pub fn acquire(
        chain: SwapChain<Device>,
        device: &Device,
        context: &mut Device::Context,
    ) -> Result<Option<Self>, Error> {
        // Select the consumer before taking ownership from the shared chain.
        device.make_context_current(context)?;
        let Some(surface) = chain.lock().take_surface() else {
            return Ok(None);
        };
        let size = device.surface_info(&surface).size;
        match device.create_surface_texture(context, surface) {
            Ok(texture) => Ok(Some(Self {
                chain,
                texture: Some(texture),
                size,
            })),
            Err((error, surface)) => {
                chain.lock().recycle_surface(surface);
                Err(error)
            },
        }
    }
    /// Actual size of the borrowed surface.
    pub fn size(&self) -> Size2D<i32> {
        self.size
    }
    /// Imported texture, present until a successful release.
    pub fn texture(&self) -> Option<&Device::SurfaceTexture> {
        self.texture.as_ref()
    }
    /// Return the loan. A failed export retains the exact native texture here.
    pub fn release(&mut self, device: &Device, context: &mut Device::Context) -> Result<(), Error> {
        let Some(texture) = self.texture.take() else {
            return Ok(());
        };
        match device.destroy_surface_texture(context, texture) {
            Ok(surface) => {
                self.chain.lock().recycle_surface(surface);
                Ok(())
            },
            Err((error, texture)) => {
                self.texture = Some(texture);
                Err(error)
            },
        }
    }
}

/// A thread-safe swap chain.
pub struct SwapChain<Device: DeviceAPI>(Arc<Mutex<SwapChainData<Device>>>);

// We can't derive Clone unfortunately
impl<Device: DeviceAPI> Clone for SwapChain<Device> {
    fn clone(&self) -> Self {
        SwapChain(self.0.clone())
    }
}

impl<Device: DeviceAPI> SwapChain<Device> {
    // Guarantee unique access to the swap chain data
    fn lock(&self) -> MutexGuard<'_, SwapChainData<Device>> {
        self.0.lock().unwrap_or_else(|err| err.into_inner())
    }

    /// Swap the back and front buffers.
    /// Called by the producer.
    /// Returns an error if `context` is not the producer context for this swap chain.
    pub fn swap_buffers(
        &self,
        device: &Device,
        context: &mut Device::Context,
        preserve_buffer: PreserveBuffer<'_>,
    ) -> Result<(), Error> {
        self.lock().swap_buffers(device, context, preserve_buffer)
    }

    /// Swap the attached swap chain.
    /// Called by the producer.
    /// Returns an error if `context` is not the producer context for both swap chains.
    /// Returns an error if this swap chain is attached, or the other swap chain is detached.
    pub fn take_attachment_from(
        &self,
        device: &Device,
        context: &mut Device::Context,
        other: &SwapChain<Device>,
    ) -> Result<(), Error> {
        if Arc::ptr_eq(&self.0, &other.0) {
            return Err(Error::Failed);
        }
        self.lock()
            .take_attachment_from(device, context, &mut *other.lock())
    }

    /// Resize the swap chain.
    /// This creates a new back buffer of the appropriate size,
    /// and destroys the old one.
    /// Called by the producer.
    /// Returns an error if `context` is not the producer context for this swap chain.
    pub fn resize(
        &self,
        device: &Device,
        context: &mut Device::Context,
        size: Size2D<i32>,
    ) -> Result<(), Error> {
        self.lock().resize(device, context, size)
    }

    /// Get the current size.
    /// Called by a consumer.
    pub fn size(&self) -> Size2D<i32> {
        self.lock().size()
    }

    /// Retry a transient attachment failure without repeating a clear or swap.
    /// Call on the producer thread with its context available for ordinary use.
    /// This does not restore a context that was lost by the native driver.
    pub fn recover_attachment(
        &self,
        device: &Device,
        context: &mut Device::Context,
    ) -> Result<(), Error> {
        self.lock().recover_attachment(device, context)
    }

    /// Take the current back buffer.
    /// Called by a producer.
    pub fn take_surface_texture(
        &self,
        device: &Device,
        context: &mut Device::Context,
    ) -> Result<Device::SurfaceTexture, Error> {
        self.lock().take_surface_texture(device, context)
    }

    /// Recycle the current back buffer.
    /// Called by a producer.
    /// The producer retains a failed native texture in this slot for retry.
    /// Once consumed, the slot is `None`; any failed native rebind is then
    /// retained by the swap chain as an unbound surface. Native texture wrappers
    /// stay on the producing thread and never enter the shared swap chain.
    pub fn recycle_surface_texture(
        &self,
        device: &Device,
        context: &mut Device::Context,
        surface_texture: &mut Option<Device::SurfaceTexture>,
    ) -> Result<(), Error> {
        self.lock()
            .recycle_surface_texture(device, context, surface_texture)
    }

    /// Take the current front buffer.
    /// Returns `None` if there is no current front buffer.
    /// Called by a consumer.
    pub fn take_pending_surface(&self) -> Option<Device::Surface> {
        self.lock().take_pending_surface()
    }

    /// Clear the current back buffer.
    /// Called by the producer.
    /// Returns an error if `context` is not the producer context for this swap chain.
    pub fn clear_surface(
        &self,
        device: &Device,
        context: &mut Device::Context,
        gl: &Gl,
        color: [f32; 4],
    ) -> Result<(), Error> {
        self.lock().clear_surface(device, context, gl, color)
    }

    /// Is this the attached swap chain?
    pub fn is_attached(&self) -> bool {
        self.lock().is_attached()
    }

    /// Destroy the swap chain.
    /// Called by the producer.
    /// Returns an error if `context` is not the producer context for this swap chain.
    pub fn destroy(&self, device: &Device, context: &mut Device::Context) -> Result<(), Error> {
        self.lock().destroy(device, context)
    }

    /// Create a new attached swap chain
    pub fn create_attached(
        device: &Device,
        context: &mut Device::Context,
        surface_access: SurfaceAccess,
    ) -> Result<SwapChain<Device>, Error> {
        let size = device
            .context_surface_info(context)?
            .ok_or(Error::Failed)?
            .size;
        Ok(SwapChain(Arc::new(Mutex::new(SwapChainData {
            size,
            context_id: device.context_id(context),
            surface_access,
            back_buffer: BackBuffer::Attached,
            pending_surface: None,
            recycled_surfaces: Vec::new(),
            retired_surfaces: Vec::new(),
            temporary_attachment: None,
            consumer_loans: 0,
            closing: false,
        }))))
    }

    /// Create a new detached swap chain
    pub fn create_detached(
        device: &Device,
        context: &mut Device::Context,
        surface_access: SurfaceAccess,
        size: Size2D<i32>,
    ) -> Result<SwapChain<Device>, Error> {
        let surface_type = SurfaceType::Generic { size };
        let surface = device.create_surface(context, surface_access, surface_type)?;
        Ok(SwapChain(Arc::new(Mutex::new(SwapChainData {
            size,
            context_id: device.context_id(context),
            surface_access,
            back_buffer: BackBuffer::Detached(surface),
            pending_surface: None,
            recycled_surfaces: Vec::new(),
            retired_surfaces: Vec::new(),
            temporary_attachment: None,
            consumer_loans: 0,
            closing: false,
        }))))
    }
}

impl<Device> SwapChainAPI for SwapChain<Device>
where
    Device: 'static + DeviceAPI,
    Device::Surface: Send,
{
    type Surface = Device::Surface;

    /// Take the current front buffer.
    /// Returns the most recent recycled surface if there is no current front buffer.
    /// Called by a consumer.
    fn take_surface(&self) -> Option<Device::Surface> {
        self.lock().take_surface()
    }

    /// Recycle the current front buffer.
    /// Called by a consumer.
    fn recycle_surface(&self, surface: Device::Surface) {
        self.lock().recycle_surface(surface)
    }
}

/// A thread-safe collection of swap chains.
#[derive(Default)]
pub struct SwapChains<SwapChainID: Eq + Hash, Device: DeviceAPI> {
    // The swap chain ids, indexed by context id
    ids: Arc<Mutex<FnvHashMap<ContextID, FnvHashSet<SwapChainID>>>>,
    // The swap chains, indexed by swap chain id
    table: Arc<RwLock<FnvHashMap<SwapChainID, SwapChain<Device>>>>,
}

// We can't derive Clone unfortunately
impl<SwapChainID: Eq + Hash, Device: DeviceAPI> Clone for SwapChains<SwapChainID, Device> {
    fn clone(&self) -> Self {
        SwapChains {
            ids: self.ids.clone(),
            table: self.table.clone(),
        }
    }
}

impl<SwapChainID, Device> SwapChains<SwapChainID, Device>
where
    SwapChainID: Clone + Eq + Hash + Debug,
    Device: DeviceAPI,
{
    /// Create a new collection.
    pub fn new() -> SwapChains<SwapChainID, Device> {
        SwapChains {
            ids: Arc::new(Mutex::new(FnvHashMap::default())),
            table: Arc::new(RwLock::new(FnvHashMap::default())),
        }
    }

    // Lock the ids
    fn ids(&self) -> MutexGuard<'_, FnvHashMap<ContextID, FnvHashSet<SwapChainID>>> {
        self.ids.lock().unwrap_or_else(|err| err.into_inner())
    }

    // Lock the lookup table
    fn table(&self) -> RwLockReadGuard<'_, FnvHashMap<SwapChainID, SwapChain<Device>>> {
        self.table.read().unwrap_or_else(|err| err.into_inner())
    }

    // Lock the lookup table for writing
    fn table_mut(&self) -> RwLockWriteGuard<'_, FnvHashMap<SwapChainID, SwapChain<Device>>> {
        self.table.write().unwrap_or_else(|err| err.into_inner())
    }

    /// Create a new attached swap chain and insert it in the table.
    /// Returns an error if the `id` is already in the table.
    pub fn create_attached_swap_chain(
        &self,
        id: SwapChainID,
        device: &Device,
        context: &mut Device::Context,
        surface_access: SurfaceAccess,
    ) -> Result<(), Error> {
        match self.table_mut().entry(id.clone()) {
            Entry::Occupied(_) => Err(Error::Failed)?,
            Entry::Vacant(entry) => {
                entry.insert(SwapChain::create_attached(device, context, surface_access)?)
            },
        };
        self.ids()
            .entry(device.context_id(context))
            .or_default()
            .insert(id);
        Ok(())
    }

    /// Create a new dettached swap chain and insert it in the table.
    /// Returns an error if the `id` is already in the table.
    pub fn create_detached_swap_chain(
        &self,
        id: SwapChainID,
        size: Size2D<i32>,
        device: &Device,
        context: &mut Device::Context,
        surface_access: SurfaceAccess,
    ) -> Result<(), Error> {
        match self.table_mut().entry(id.clone()) {
            Entry::Occupied(_) => Err(Error::Failed)?,
            Entry::Vacant(entry) => entry.insert(SwapChain::create_detached(
                device,
                context,
                surface_access,
                size,
            )?),
        };
        self.ids()
            .entry(device.context_id(context))
            .or_default()
            .insert(id);
        Ok(())
    }

    /// Destroy a swap chain.
    /// Called by the producer.
    /// Returns an error if `context` is not the producer context for the swap chain.
    pub fn destroy(
        &self,
        id: SwapChainID,
        device: &Device,
        context: &mut Device::Context,
    ) -> Result<(), Error> {
        let swap_chain = self.table().get(&id).cloned();
        if let Some(swap_chain) = swap_chain {
            swap_chain.destroy(device, context)?;
        }
        self.table_mut().remove(&id);
        if let Some(ids) = self.ids().get_mut(&device.context_id(context)) {
            ids.remove(&id);
        }
        Ok(())
    }

    /// Iterate over all the swap chains for a particular producer context.
    /// Called by the producer.
    pub fn iter(
        &self,
        device: &Device,
        context: &mut Device::Context,
    ) -> impl Iterator<Item = (SwapChainID, SwapChain<Device>)> {
        self.ids()
            .get(&device.context_id(context))
            .iter()
            .flat_map(|ids| ids.iter())
            .filter_map(|id| Some((id.clone(), self.table().get(id)?.clone())))
            .collect::<Vec<_>>()
            .into_iter()
    }
}

impl<SwapChainID, Device> SwapChainsAPI<SwapChainID> for SwapChains<SwapChainID, Device>
where
    SwapChainID: 'static + Clone + Eq + Hash + Debug + Sync + Send,
    Device: 'static + DeviceAPI,
    Device::Surface: Send,
{
    type Surface = Device::Surface;
    type SwapChain = SwapChain<Device>;

    /// Get a swap chain
    fn get(&self, id: SwapChainID) -> Option<SwapChain<Device>> {
        debug!("Getting swap chain {:?}", id);
        self.table().get(&id).cloned()
    }
}

/// The consumer's view of a swap chain
pub trait SwapChainAPI: 'static + Clone + Send {
    type Surface;

    /// Take the current front buffer.
    fn take_surface(&self) -> Option<Self::Surface>;

    /// Recycle the current front buffer.
    fn recycle_surface(&self, surface: Self::Surface);
}

/// The consumer's view of a collection of swap chains
pub trait SwapChainsAPI<SwapChainID>: 'static + Clone + Send {
    type Surface;
    type SwapChain: SwapChainAPI<Surface = Self::Surface>;

    /// Get a swap chain
    fn get(&self, id: SwapChainID) -> Option<Self::SwapChain>;
}
