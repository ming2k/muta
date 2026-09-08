//! Application component composition. Structural behavior belongs to the
//! engine runtime; this module supplies typed identities and surface policy.

use mutx_engine::ui::{Component, InputPolicy, LayoutBox, PointerPolicy, Scene, UiRuntime};
use mutx_engine::Rect;

use crate::model::layout::{LayoutMap, PermissionActionHit, QuestionOptionHit};
use crate::sheet::SheetKind;
use crate::Modal;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum UiKey {
    Root,
    Transcript,
    Footer,
    Composer,
    Queue,
    Activity,
    ModelBar,
    Context,
    Performance,
    Connection,
    Sticky,
    Backdrop,
    Sheet(SheetKind),
    QuestionOption(usize),
    PermissionAction(usize),
    Completion,
    CompletionItem(usize),
    Modal(Modal),
    ProviderDelete,
    OauthUrl,
    OauthCode,
    Toast,
    PreAttach,
}

/// The application has one mounted UI runtime. Semantic text mappings travel
/// with its frame transaction but remain document data, not UI hit targets.
pub struct ComponentTree {
    pub runtime: UiRuntime<UiKey>,
    pub document: LayoutMap,
    pending_document: Option<LayoutMap>,
    modal: Modal,
}

impl Default for ComponentTree {
    fn default() -> Self {
        Self {
            runtime: UiRuntime::default(),
            document: LayoutMap::new(),
            pending_document: None,
            modal: Modal::None,
        }
    }
}

impl ComponentTree {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn begin(&mut self, viewport: Rect, modal: Modal) {
        self.modal = modal;
        self.pending_document = None;
        self.runtime.begin(viewport);
        self.mount(UiKey::Root, viewport);
        self.mount(UiKey::Transcript, viewport);
        self.mount(UiKey::Footer, viewport);
        self.mount(UiKey::Composer, Rect::default());
        if modal != Modal::None {
            self.mount(UiKey::Modal(modal), viewport);
        }
    }

    pub fn stage_document(&mut self, document: LayoutMap) {
        self.pending_document = Some(document);
    }

    pub fn commit(&mut self) {
        if let Err(error) = self.runtime.commit() {
            panic!("invalid application component frame: {error}");
        }
        if let Some(document) = self.pending_document.take() {
            self.document = document;
        }
    }

    pub fn scene(&self) -> &Scene<UiKey> {
        self.runtime.presented()
    }

    pub fn bounds(&self, key: UiKey) -> Option<Rect> {
        self.scene().rect(&key).filter(|rect| rect.area() > 0)
    }

    pub fn contains(&self, key: UiKey, x: u16, y: u16) -> bool {
        self.scene().layout(&key).is_some_and(|layout| layout.clip.contains(x, y))
    }

    pub fn target(&self, x: u16, y: u16) -> Option<UiKey> {
        self.runtime.pointer_target(x, y).copied()
    }

    pub fn modal_bounds(&self) -> Option<Rect> {
        self.scene().paint_order().rev().find_map(|(key, layout)| {
            matches!(key, UiKey::Modal(_)).then_some(layout.bounds)
        })
    }

    pub fn mount(&mut self, key: UiKey, rect: Rect) {
        let (parent, layer, pointer, input, focusable) = match key {
            UiKey::Root => (None, 0, PointerPolicy::Transparent, InputPolicy::None, false),
            UiKey::Transcript => (Some(UiKey::Root), 1, PointerPolicy::Target, InputPolicy::Bubble, true),
            UiKey::Footer => (Some(UiKey::Root), 2, PointerPolicy::Transparent, InputPolicy::None, false),
            UiKey::Composer => (Some(UiKey::Footer), 3, PointerPolicy::Target, InputPolicy::Bubble, true),
            UiKey::Queue | UiKey::Activity | UiKey::ModelBar => (Some(UiKey::Footer), 3, PointerPolicy::Target, InputPolicy::None, false),
            UiKey::Context | UiKey::Performance | UiKey::Connection => (Some(UiKey::Footer), 4, PointerPolicy::Target, InputPolicy::None, false),
            UiKey::Sticky => (Some(UiKey::Transcript), 5, PointerPolicy::Target, InputPolicy::None, false),
            UiKey::Backdrop => (Some(UiKey::Root), 25, PointerPolicy::Transparent, InputPolicy::None, false),
            UiKey::Sheet(_) => (Some(UiKey::Footer), 10, PointerPolicy::Target, InputPolicy::Scope(u64::MAX), true),
            UiKey::QuestionOption(_) => (Some(UiKey::Sheet(SheetKind::Question)), 11, PointerPolicy::Target, InputPolicy::Bubble, true),
            UiKey::PermissionAction(_) => (Some(UiKey::Sheet(SheetKind::Permission)), 11, PointerPolicy::Target, InputPolicy::Bubble, true),
            UiKey::Completion => (Some(UiKey::Composer), 20, PointerPolicy::Target, InputPolicy::Bubble, false),
            UiKey::CompletionItem(_) => (Some(UiKey::Completion), 21, PointerPolicy::Target, InputPolicy::Bubble, false),
            UiKey::Modal(_) => (Some(UiKey::Root), 30, PointerPolicy::Barrier, InputPolicy::Modal, true),
            UiKey::OauthUrl | UiKey::OauthCode => (Some(UiKey::Modal(Modal::OauthPending)), 31, PointerPolicy::Target, InputPolicy::None, false),
            UiKey::ProviderDelete => (Some(UiKey::Modal(Modal::Connections)), 40, PointerPolicy::Barrier, InputPolicy::Modal, true),
            UiKey::Toast => (Some(UiKey::Root), 50, PointerPolicy::Transparent, InputPolicy::None, false),
            UiKey::PreAttach => (Some(UiKey::Root), 60, PointerPolicy::Barrier, InputPolicy::Modal, true),
        };
        // Popups and sheets retain logical ownership while escaping the
        // composer's narrow clip. All other children inherit clipping.
        let layout = if matches!(key, UiKey::Sheet(_) | UiKey::Completion | UiKey::ProviderDelete) {
            LayoutBox::Viewport(rect)
        } else { LayoutBox::Placed(rect) };
        let result = if self.runtime.pending().is_ok_and(|scene| scene.id(&key).is_some()) {
            self.runtime.place(&key, layout)
        } else {
            let mut component = Component::new(key, parent, layout).layer(layer);
            component.pointer = pointer;
            component.input = input;
            component.focusable = focusable;
            self.runtime.mount(component)
        };
        if let Err(error) = result {
            panic!("invalid component {key:?}: {error}");
        }
    }

    pub fn mount_completion(&mut self, rect: Rect) { self.mount(UiKey::Completion, rect); }

    pub fn paint<R>(&self, frame: &mut mutx_engine::Frame<'_>, key: UiKey,
        painter: impl FnOnce(&mut mutx_engine::Frame<'_>) -> R) -> R {
        self.runtime.paint_node(&key, frame, painter)
            .unwrap_or_else(|error| panic!("cannot paint {key:?}: {error}"))
    }
    pub fn mount_completion_item(&mut self, index: usize, rect: Rect) { self.mount(UiKey::CompletionItem(index), rect); }
    pub fn mount_question_option(&mut self, hit: QuestionOptionHit) { self.mount(UiKey::QuestionOption(hit.option_index), hit.rect); }
    pub fn mount_permission_action(&mut self, hit: PermissionActionHit) { self.mount(UiKey::PermissionAction(hit.action_index), hit.rect); }
    pub fn mount_permission_sheet(&mut self, rect: Rect) { self.mount(UiKey::Sheet(SheetKind::Permission), rect); }
    pub fn mount_oauth_url(&mut self, rect: Rect) { self.mount(UiKey::OauthUrl, rect); }
    pub fn mount_oauth_code(&mut self, rect: Rect) { self.mount(UiKey::OauthCode, rect); }
    pub fn mount_oauth_modal(&mut self, rect: Rect) { self.mount(UiKey::Modal(Modal::OauthPending), rect); }

    pub fn completion_item_at(&self, x: u16, y: u16) -> Option<usize> {
        match self.target(x, y) { Some(UiKey::CompletionItem(i)) => Some(i), _ => None }
    }
    pub fn completion_menu_contains(&self, x: u16, y: u16) -> bool { self.contains(UiKey::Completion, x, y) }
    pub fn permission_sheet_contains(&self, x: u16, y: u16) -> bool { self.contains(UiKey::Sheet(SheetKind::Permission), x, y) }
    pub fn oauth_modal_contains(&self, x: u16, y: u16) -> bool { self.contains(UiKey::Modal(Modal::OauthPending), x, y) }
    pub fn question_option_at(&self, x: u16, y: u16) -> Option<QuestionOptionHit> {
        match self.target(x, y) {
            Some(UiKey::QuestionOption(option_index)) => self.bounds(UiKey::QuestionOption(option_index)).map(|rect| QuestionOptionHit { option_index, rect }),
            _ => None,
        }
    }
    pub fn permission_action_at(&self, x: u16, y: u16) -> Option<PermissionActionHit> {
        match self.target(x, y) {
            Some(UiKey::PermissionAction(action_index)) => self.bounds(UiKey::PermissionAction(action_index)).map(|rect| PermissionActionHit { action_index, rect }),
            _ => None,
        }
    }
}
