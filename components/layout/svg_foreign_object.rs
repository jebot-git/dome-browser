/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/. */

//! CSS layout islands for HTML in an inline SVG foreignObject. Keeping their
//! fragments in the ordinary tree gives painting, hit testing, DOM geometry,
//! native event targeting, selection and focus the same coordinate system.

use std::str::FromStr;
use std::sync::Arc;

use app_units::Au;
use layout_api::{LayoutElement, LayoutNode};
use malloc_size_of_derive::MallocSizeOf;
use script::layout_dom::ServoLayoutNode;
use servo_arc::Arc as ServoArc;
use style::Zero;
use style::properties::ComputedValues;
use style::selector_parser::PseudoElement;
use style::values::computed::transform::{Matrix, Transform, TransformOperation, TransformOrigin};
use style::values::generics::transform::GenericTransform;
use web_atoms::{LocalName, ns};

use crate::context::LayoutContext;
use crate::dom::{LayoutBox, NodeExt, WeakLayoutBox};
use crate::dom_traversal::{Contents, NodeAndStyleInfo, NonReplacedContents};
use crate::flow::BlockLevelBox;
use crate::formatting_contexts::IndependentFormattingContext;
use crate::fragment_tree::{BaseFragmentInfo, BoxFragment, Fragment, FragmentFlags};
use crate::geom::{LogicalVec2, PhysicalPoint, PhysicalRect, PhysicalSides, PhysicalSize};
use crate::positioned::PositioningContext;
use crate::sizing::{LazySize, SizeConstraint};
use crate::style_ext::{Display, DisplayInside};
use crate::{ArcRefCell, ContainingBlock, ContainingBlockSize, PropagatedBoxTreeData};

#[derive(Clone, Copy, Debug, MallocSizeOf)]
struct Length {
    value: f64,
    percent: bool,
}
impl Length {
    fn parse(value: Option<&str>, default: f64) -> Option<Self> {
        let Some(value) = value else {
            return Some(Self {
                value: default,
                percent: false,
            });
        };
        let length = svgtypes::Length::from_str(value).ok()?;
        let (value, percent) = match length.unit {
            svgtypes::LengthUnit::None | svgtypes::LengthUnit::Px => (length.number, false),
            svgtypes::LengthUnit::Percent => (length.number / 100., true),
            svgtypes::LengthUnit::In => (length.number * 96., false),
            svgtypes::LengthUnit::Cm => (length.number * 96. / 2.54, false),
            svgtypes::LengthUnit::Mm => (length.number * 96. / 25.4, false),
            svgtypes::LengthUnit::Pt => (length.number * 96. / 72., false),
            svgtypes::LengthUnit::Pc => (length.number * 16., false),
            _ => return None,
        };
        value.is_finite().then_some(Self { value, percent })
    }
    fn resolve(self, basis: f64) -> f64 {
        self.value * if self.percent { basis } else { 1. }
    }
}

#[derive(Clone, Copy, Debug, MallocSizeOf)]
struct Affine([f64; 6]);
impl Affine {
    const IDENTITY: Self = Self([1., 0., 0., 1., 0., 0.]);
    fn then(self, next: Self) -> Self {
        let [a, b, c, d, e, f] = self.0;
        let [g, h, i, j, k, l] = next.0;
        Self([
            g * a + i * b,
            h * a + j * b,
            g * c + i * d,
            h * c + j * d,
            g * e + i * f + k,
            h * e + j * f + l,
        ])
    }
    fn translate(x: f64, y: f64) -> Self {
        Self([1., 0., 0., 1., x, y])
    }
    fn from_attribute(value: Option<&str>) -> Option<Self> {
        let Some(value) = value else {
            return Some(Self::IDENTITY);
        };
        let t = svgtypes::Transform::from_str(value).ok()?;
        let result = Self([t.a, t.b, t.c, t.d, t.e, t.f]);
        result.0.iter().all(|v| v.is_finite()).then_some(result)
    }
}

#[derive(Debug, MallocSizeOf)]
pub(crate) struct ForeignObjects {
    #[ignore_malloc_size_of = "viewBox numbers are stored inline"]
    view_box: Option<[f64; 4]>,
    align: [f64; 2],
    preserve_ratio: bool,
    slice: bool,
    objects: Vec<ForeignObject>,
}
#[derive(Debug, MallocSizeOf)]
struct ForeignObject {
    rect: [Length; 4],
    transform: Affine,
    context: ArcRefCell<BlockLevelBox>,
    wrapper_style: ServoArc<ComputedValues>,
}

fn attribute(node: &ServoLayoutNode<'_>, name: &str) -> Option<String> {
    node.as_element()?
        .attribute_as_str(&ns!(), &LocalName::from(name))
        .map(str::to_owned)
}

impl ForeignObjects {
    pub(crate) fn construct(node: ServoLayoutNode<'_>, context: &LayoutContext) -> Self {
        let view_box_attribute = attribute(&node, "viewBox");
        let view_box = view_box_attribute
            .as_deref()
            .and_then(|v| svgtypes::ViewBox::from_str(v).ok())
            .map(|v| [v.x, v.y, v.w, v.h]);
        let aspect =
            attribute(&node, "preserveAspectRatio").unwrap_or_else(|| "xMidYMid meet".into());
        let align = [
            if aspect.contains("xMin") {
                0.
            } else if aspect.contains("xMax") {
                1.
            } else {
                0.5
            },
            if aspect.contains("YMin") {
                0.
            } else if aspect.contains("YMax") {
                1.
            } else {
                0.5
            },
        ];
        let mut result = Self {
            view_box,
            align,
            preserve_ratio: !aspect.contains("none"),
            slice: aspect.contains("slice"),
            objects: Vec::new(),
        };
        if view_box_attribute.is_some() && view_box.is_none() {
            return result;
        }
        for child in node.dom_children() {
            result.collect(child, context, Affine::IDENTITY);
        }
        result
    }

    fn collect(&mut self, node: ServoLayoutNode<'_>, context: &LayoutContext, parent: Affine) {
        let Some(element) = node.as_element() else {
            return;
        };
        if element.namespace() != &ns!(svg) {
            return;
        }
        let style = node.style(&context.style_context);
        if matches!(Display::from(style.get_box().display), Display::None) {
            return;
        }
        let Some(transform) = Affine::from_attribute(attribute(&node, "transform").as_deref())
        else {
            return;
        };
        let transform = transform.then(parent);
        match element.local_name().as_ref() {
            "foreignObject" => {
                let rect = [
                    Length::parse(attribute(&node, "x").as_deref(), 0.),
                    Length::parse(attribute(&node, "y").as_deref(), 0.),
                    Length::parse(attribute(&node, "width").as_deref(), 0.),
                    Length::parse(attribute(&node, "height").as_deref(), 0.),
                ];
                let [Some(x), Some(y), Some(width), Some(height)] = rect else {
                    return;
                };
                let info = NodeAndStyleInfo::new(node, style);
                let wrapper_style = info
                    .with_pseudo_element(context, PseudoElement::ServoAnonymousBox)
                    .expect("anonymous SVG viewport style")
                    .style;
                let mut flow = IndependentFormattingContext::construct(
                    context,
                    &info,
                    DisplayInside::FlowRoot {
                        is_list_item: false,
                    },
                    Contents::NonReplaced(NonReplacedContents::OfElement),
                    PropagatedBoxTreeData::default(),
                );
                flow.base
                    .base_fragment_info
                    .flags
                    .insert(FragmentFlags::IS_SVG_FOREIGN_OBJECT);
                let layout_box = ArcRefCell::new(BlockLevelBox::Independent(flow));
                node.box_slot()
                    .set(LayoutBox::BlockLevel(layout_box.clone()));
                self.objects.push(ForeignObject {
                    rect: [x, y, width, height],
                    transform,
                    context: layout_box,
                    wrapper_style,
                });
            },
            "g" | "a" => {
                for child in node.dom_children() {
                    self.collect(child, context, transform);
                }
            },
            // Non-rendered definitions must never create CSS hit targets.
            // Nested SVG viewports need a separate viewport transform and clip.
            _ => {},
        }
    }

    pub(crate) fn attached_to_tree(&self, parent: WeakLayoutBox) {
        for object in &self.objects {
            object.context.borrow_mut().with_base_mut(|base| {
                base.parent_box = Some(parent.clone());
            });
        }
    }

    pub(crate) fn subtree_size(&self) -> usize {
        self.objects
            .iter()
            .map(|object| {
                object
                    .context
                    .borrow()
                    .with_base(|base| base.subtree_size())
            })
            .sum()
    }

    pub(crate) fn fragments(
        &self,
        context: &LayoutContext,
        viewport: PhysicalRect<Au>,
    ) -> Vec<Fragment> {
        let width = viewport.size.width.to_f64_px();
        let height = viewport.size.height.to_f64_px();
        if width <= 0. || height <= 0. {
            return vec![];
        }
        let [vx, vy, vw, vh] = self.view_box.unwrap_or([0., 0., width, height]);
        if vw <= 0. || vh <= 0. {
            return vec![];
        }
        let mut sx = width / vw;
        let mut sy = height / vh;
        if self.preserve_ratio {
            sx = if self.slice { sx.max(sy) } else { sx.min(sy) };
            sy = sx;
        }
        let root = Affine([
            sx,
            0.,
            0.,
            sy,
            (width - vw * sx) * self.align[0] - vx * sx + viewport.origin.x.to_f64_px(),
            (height - vh * sy) * self.align[1] - vy * sy + viewport.origin.y.to_f64_px(),
        ]);
        let mut fragments = vec![];
        for object in &self.objects {
            let [x, y, w, h] = object.rect;
            let x = x.resolve(vw);
            let y = y.resolve(vh);
            let w = w.resolve(vw);
            let h = h.resolve(vh);
            if w <= 0. || h <= 0. || ![x, y, w, h].iter().all(|v| v.is_finite()) {
                continue;
            }
            let bounds = PhysicalSize::new(Au::from_f64_px(w), Au::from_f64_px(h));
            let layout_box = object.context.borrow();
            let BlockLevelBox::Independent(flow) = &*layout_box else {
                continue;
            };
            let mut positioning = PositioningContext::default();
            let size = LogicalVec2::from_physical_size(&bounds, flow.base.style.writing_mode);
            let cb = ContainingBlock {
                size: ContainingBlockSize {
                    inline: size.inline,
                    block: SizeConstraint::Definite(size.block),
                },
                style: &flow.base.style,
            };
            let mut layout = flow.layout(
                context,
                &mut positioning,
                &cb,
                &cb,
                None,
                &LazySize::intrinsic(),
            );
            let mut fragment = BoxFragment::new(
                flow.base.base_fragment_info,
                flow.base.style.clone(),
                std::mem::take(&mut layout.fragments),
                PhysicalRect::new(PhysicalPoint::origin(), bounds),
                PhysicalSides::zero(),
                PhysicalSides::zero(),
                PhysicalSides::zero(),
                None,
            );
            positioning.layout_collected_children(context, &mut fragment);
            let fragment = Fragment::Box(Arc::new(fragment));
            flow.base.set_fragment(fragment.clone());
            // Keep SVG's viewport transform on an anonymous parent. A normal
            // style-only repair of foreignObject must not erase that transform.
            let mut style: ComputedValues = (*object.wrapper_style).clone();
            style
                .mutate_box()
                .set_display(style::values::specified::box_::Display::Block);
            let matrix_values = Affine::translate(x, y).then(object.transform).then(root).0;
            if !matrix_values
                .iter()
                .all(|v| v.is_finite() && v.abs() <= f32::MAX as f64)
            {
                continue;
            }
            let [a, b, c, d, e, f] = matrix_values;
            let matrix = TransformOperation::Matrix(Matrix {
                a: a as f32,
                b: b as f32,
                c: c as f32,
                d: d as f32,
                e: e as f32,
                f: f as f32,
            });
            let transform: Transform = GenericTransform(vec![matrix].into());
            style.mutate_box().set_transform(transform);
            style
                .mutate_box()
                .set_transform_origin(TransformOrigin::new(
                    Zero::zero(),
                    Zero::zero(),
                    Zero::zero(),
                ));
            let wrapper = BoxFragment::new(
                BaseFragmentInfo::anonymous(),
                ServoArc::new(style),
                vec![fragment],
                PhysicalRect::new(PhysicalPoint::origin(), bounds),
                PhysicalSides::zero(),
                PhysicalSides::zero(),
                PhysicalSides::zero(),
                None,
            );
            fragments.push(Fragment::Box(Arc::new(wrapper)));
        }
        fragments
    }
}
