use std::collections::HashMap;
use std::hash::Hash;

use crate::{Flex, FlexItem, Rect};

/// An instance token. Tokens are never reused, including after aborted frames.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct NodeId(pub(super) u64);

/// Input ownership is independent from whether a node paints cells.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum InputPolicy {
    #[default]
    None,
    /// Receives unhandled keyboard events from descendants.
    Bubble,
    /// Owns declared event families without trapping focus or pointer input.
    /// Families are application-defined bits; the runtime interprets only
    /// their intersection with the dispatched event's family.
    Scope(u64),
    /// The highest painted scope blocks keyboard input to lower scopes.
    Modal,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum PointerPolicy {
    #[default]
    Transparent,
    Target,
    /// Blocks every point in the viewport, including outside its own bounds.
    /// Descendants remain hittable; outside clicks target this barrier.
    Barrier,
}

/// Placement is separate from logical ownership. Viewport placement is the
/// explicit escape hatch for popups that outgrow their owner's clipping box.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LayoutBox {
    Fill,
    /// Absolute terminal coordinates, clipped by the parent's content box.
    Placed(Rect),
    /// Absolute terminal coordinates clipped only by the viewport.
    Viewport(Rect),
}

impl Default for LayoutBox {
    fn default() -> Self {
        Self::Fill
    }
}

/// A declaration, not the lifetime owner of a component's state.
#[derive(Clone, Debug)]
pub struct Component<K> {
    pub key: K,
    pub parent: Option<K>,
    pub layout: LayoutBox,
    pub input: InputPolicy,
    pub pointer: PointerPolicy,
    pub focusable: bool,
    /// Global paint layer; declaration order breaks ties.
    pub layer: i32,
    /// A changed revision invalidates painting, never measurement by itself.
    pub revision: u64,
}

impl<K> Component<K> {
    pub fn new(key: K, parent: Option<K>, layout: LayoutBox) -> Self {
        Self {
            key,
            parent,
            layout,
            input: InputPolicy::None,
            pointer: PointerPolicy::Transparent,
            focusable: false,
            layer: 0,
            revision: 0,
        }
    }

    pub fn interactive(mut self) -> Self {
        self.pointer = PointerPolicy::Target;
        self.focusable = true;
        self.input = InputPolicy::Bubble;
        self
    }

    pub fn modal(mut self) -> Self {
        self.input = InputPolicy::Modal;
        self.pointer = PointerPolicy::Barrier;
        self.focusable = true;
        self
    }

    pub fn layer(mut self, layer: i32) -> Self {
        self.layer = layer;
        self
    }

    pub fn revision(mut self, revision: u64) -> Self {
        self.revision = revision;
        self
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct NodeLayout {
    pub bounds: Rect,
    pub clip: Rect,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum UiError {
    DuplicateKey,
    MissingParent,
    MissingNode,
    NoPendingFrame,
    InvalidFocus,
    InvalidCapture,
    IdentityExhausted,
}

impl std::fmt::Display for UiError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "UI runtime invariant: {self:?}")
    }
}

impl std::error::Error for UiError {}

#[derive(Clone, Debug)]
pub(super) struct Node<K> {
    pub id: NodeId,
    pub component: Component<K>,
    pub layout: NodeLayout,
}

/// A solved frame. Construction requires parents before children, preventing
/// cycles by construction. Queries never inspect application state.
#[derive(Clone, Debug)]
pub struct Scene<K> {
    pub(super) viewport: Rect,
    pub(super) nodes: Vec<Node<K>>,
    pub(super) keys: HashMap<K, usize>,
    pub(super) order: Vec<usize>,
}

impl<K> Default for Scene<K> {
    fn default() -> Self {
        Self {
            viewport: Rect::default(),
            nodes: Vec::new(),
            keys: HashMap::new(),
            order: Vec::new(),
        }
    }
}

impl<K: Clone + Eq + Hash> Scene<K> {
    pub fn viewport(&self) -> Rect {
        self.viewport
    }

    pub fn len(&self) -> usize {
        self.nodes.len()
    }

    pub fn is_empty(&self) -> bool {
        self.nodes.is_empty()
    }

    pub fn id(&self, key: &K) -> Option<NodeId> {
        self.node(key).map(|node| node.id)
    }

    pub fn key(&self, id: NodeId) -> Option<&K> {
        self.nodes.iter().find(|node| node.id == id).map(|node| &node.component.key)
    }

    pub fn layout(&self, key: &K) -> Option<NodeLayout> {
        self.node(key).map(|node| node.layout)
    }

    pub fn rect(&self, key: &K) -> Option<Rect> {
        self.layout(key).map(|layout| layout.bounds)
    }

    pub fn component(&self, key: &K) -> Option<&Component<K>> {
        self.node(key).map(|node| &node.component)
    }

    pub fn paint_order(&self) -> impl DoubleEndedIterator<Item = (&K, NodeLayout)> {
        self.order.iter().map(|&index| {
            let node = &self.nodes[index];
            (&node.component.key, node.layout)
        })
    }

    pub fn hit_test(&self, x: u16, y: u16) -> Option<&K> {
        if !self.viewport.contains(x, y) {
            return None;
        }
        for &index in self.order.iter().rev() {
            let node = &self.nodes[index];
            match node.component.pointer {
                PointerPolicy::Barrier => return Some(&node.component.key),
                PointerPolicy::Target if node.layout.clip.contains(x, y) => {
                    return Some(&node.component.key);
                }
                _ => {}
            }
        }
        None
    }

    pub fn foreground(&self) -> Option<&K> {
        self.foreground_for(u64::MAX)
    }

    pub fn foreground_for(&self, family: u64) -> Option<&K> {
        self.order.iter().rev().find_map(|&index| {
            let node = &self.nodes[index];
            match node.component.input {
                InputPolicy::Modal => Some(&node.component.key),
                InputPolicy::Scope(claims) if claims & family != 0 => Some(&node.component.key),
                _ => None,
            }
        })
    }

    pub(super) fn focus_barrier(&self) -> Option<&K> {
        self.order.iter().rev().find_map(|&i| {
            let node = &self.nodes[i];
            (node.component.input == InputPolicy::Modal).then_some(&node.component.key)
        })
    }

    pub(super) fn pointer_barrier(&self) -> Option<&K> {
        self.order.iter().rev().find_map(|&i| {
            let node = &self.nodes[i];
            (node.component.pointer == PointerPolicy::Barrier).then_some(&node.component.key)
        })
    }

    pub fn is_descendant(&self, child: &K, ancestor: &K) -> bool {
        let mut cursor = Some(child);
        while let Some(key) = cursor {
            if key == ancestor {
                return true;
            }
            cursor = self.node(key).and_then(|node| node.component.parent.as_ref());
        }
        false
    }

    /// Keyboard ancestry terminates at the modal barrier. Unhandled does not
    /// mean that a modal permits delivery to a visually covered sibling.
    pub fn keyboard_path(&self, focused: Option<NodeId>) -> Vec<&K> {
        self.keyboard_path_for(focused, u64::MAX)
    }

    pub fn keyboard_path_for(&self, focused: Option<NodeId>, family: u64) -> Vec<&K> {
        let barrier = self.foreground_for(family);
        let target = focused.and_then(|id| self.key(id)).filter(|key| {
            barrier.is_none_or(|scope| self.is_descendant(key, scope))
        }).or(barrier);
        let mut path = Vec::new();
        let mut cursor = target;
        while let Some(key) = cursor {
            let Some(node) = self.node(key) else { break };
            if node.component.input != InputPolicy::None {
                path.push(key);
            }
            if matches!(node.component.input, InputPolicy::Modal | InputPolicy::Scope(_)) {
                break;
            }
            cursor = node.component.parent.as_ref();
        }
        path
    }

    /// Solve one container's children using the engine's integer flex solver.
    /// This operation performs no drawing and accepts content measurements
    /// supplied by leaf components.
    pub fn flex_children(
        &self,
        parent: &K,
        flex: &Flex,
        items: &[FlexItem],
        measure: &dyn Fn(usize, u16) -> u16,
    ) -> Result<Vec<Rect>, UiError> {
        let area = self.rect(parent).ok_or(UiError::MissingParent)?;
        let solved = flex.solve_with(area, items, measure);
        Ok((0..items.len()).map(|i| solved.rect(i)).collect())
    }

    pub(super) fn node(&self, key: &K) -> Option<&Node<K>> {
        self.keys.get(key).and_then(|&index| self.nodes.get(index))
    }

    pub(super) fn push(&mut self, id: NodeId, component: Component<K>) -> Result<NodeLayout, UiError> {
        if self.keys.contains_key(&component.key) {
            return Err(UiError::DuplicateKey);
        }
        let parent = match component.parent.as_ref() {
            Some(key) => self.layout(key).ok_or(UiError::MissingParent)?,
            None => NodeLayout { bounds: self.viewport, clip: self.viewport },
        };
        let (bounds, clip) = match component.layout {
            LayoutBox::Fill => (parent.bounds, parent.clip),
            LayoutBox::Placed(bounds) => (bounds, parent.clip.intersection(bounds)),
            LayoutBox::Viewport(bounds) => (bounds, self.viewport.intersection(bounds)),
        };
        let layout = NodeLayout { bounds, clip };
        let index = self.nodes.len();
        self.keys.insert(component.key.clone(), index);
        self.nodes.push(Node { id, component, layout });
        self.order.push(index);
        self.order.sort_by_key(|&i| (self.nodes[i].component.layer, i));
        Ok(layout)
    }
}
