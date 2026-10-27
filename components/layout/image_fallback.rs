/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/. */

//! Native text contained by a replaced image fallback box. No DOM children or
//! rasterized text are needed: normal shaping, bidi, wrapping and clips apply.

use std::sync::Arc;

use app_units::Au;
use layout_api::LayoutNode;
use malloc_size_of_derive::MallocSizeOf;
use script::layout_dom::{ServoDangerousStyleElement, ServoLayoutNode};
use servo_arc::Arc as ServoArc;
use style::selector_parser::PseudoElement;
use style::values::specified::box_::{Display, Overflow};

use crate::context::LayoutContext;
use crate::dom_traversal::{Contents, NodeAndStyleInfo, NonReplacedContents, PseudoElementContentItem};
use crate::formatting_contexts::IndependentFormattingContext;
use crate::fragment_tree::{BaseFragmentInfo, BoxFragment, Fragment};
use crate::geom::{LogicalVec2, PhysicalPoint, PhysicalRect, PhysicalSides, PhysicalSize};
use crate::positioned::PositioningContext;
use crate::sizing::{InlineContentSizesResult, LazySize, SizeConstraint};
use crate::style_ext::DisplayInside;
use crate::{ConstraintSpace, ContainingBlock, ContainingBlockSize};

#[derive(Debug, MallocSizeOf)]
pub(crate) struct ImageFallback {
    flow: crate::ArcRefCell<IndependentFormattingContext>,
}

impl ImageFallback {
    pub(crate) fn new(node: ServoLayoutNode<'_>, context: &LayoutContext, text: String) -> Self {
        let parent_style = node.style(&context.style_context);
        let anonymous_style = context.style_context.stylist
            .style_for_anonymous::<ServoDangerousStyleElement>(
                &context.style_context.guards,
                &PseudoElement::ServoAnonymousBox,
                &parent_style,
            );
        let mut style = (*anonymous_style).clone();
        style.mutate_box().set_display(Display::Block);
        style.mutate_box().set_overflow_x(Overflow::Hidden);
        style.mutate_box().set_overflow_y(Overflow::Hidden);
        let info = NodeAndStyleInfo::new(node, ServoArc::new(style));
        let mut flow = IndependentFormattingContext::construct(
            context,
            &info,
            DisplayInside::FlowRoot { is_list_item: false },
            Contents::NonReplaced(NonReplacedContents::OfPseudoElement(vec![
                PseudoElementContentItem::Text(text),
            ])),
            Default::default(),
        );
        flow.base.base_fragment_info = BaseFragmentInfo::anonymous();
        Self { flow: crate::ArcRefCell::new(flow) }
    }

    pub(crate) fn inline_sizes(&self, context: &LayoutContext) -> InlineContentSizesResult {
        let flow = self.flow.borrow();
        flow.inline_content_sizes(context, &ConstraintSpace::new(
            SizeConstraint::default(), &flow.base.style, None,
        ))
    }

    pub(crate) fn block_size(&self, context: &LayoutContext, inline: Au) -> Au {
        let flow = self.flow.borrow();
        let cb = ContainingBlock {
            size: ContainingBlockSize { inline, block: SizeConstraint::default() },
            style: &flow.base.style,
        };
        flow.layout(
            context, &mut PositioningContext::default(), &cb, &cb,
            None, &LazySize::intrinsic(),
        ).content_block_size
    }

    pub(crate) fn fragments(&self, context: &LayoutContext, size: PhysicalSize<Au>) -> Vec<Fragment> {
        let flow = self.flow.borrow();
        let logical = LogicalVec2::from_physical_size(&size, flow.base.style.writing_mode);
        let cb = ContainingBlock {
            size: ContainingBlockSize {
                inline: logical.inline, block: SizeConstraint::Definite(logical.block),
            },
            style: &flow.base.style,
        };
        let layout = flow.layout(
            context, &mut PositioningContext::default(), &cb, &cb,
            None, &LazySize::intrinsic(),
        );
        vec![Fragment::Box(Arc::new(BoxFragment::new(
            BaseFragmentInfo::anonymous(), flow.base.style.clone(), layout.fragments,
            PhysicalRect::new(PhysicalPoint::origin(), size),
            PhysicalSides::zero(), PhysicalSides::zero(), PhysicalSides::zero(), None,
        ))) ]
    }

    pub(crate) fn subtree_size(&self) -> usize {
        self.flow.borrow().base.subtree_size()
    }

    pub(crate) fn attached_to_tree(&self, parent: crate::dom::WeakLayoutBox) {
        self.flow.borrow_mut().base.parent_box = Some(parent);
    }
}
