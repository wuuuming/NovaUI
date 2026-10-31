//! What an element is to the keyboard and to assistive clients (screen readers): its role, the
//! name it is announced by, whether it takes the keyboard focus, and a slider's value. Written in
//! `.nui` (`role`, `label`, `focusable`, `value`, `min`, `max`, `step`, `live`) or by a widget; what is not
//! written is inferred from what the element does (`Tree::role`): an `on-click` is a button, a
//! text field a text input, a text a label.

/// What kind of thing an element is, as an assistive client names it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Role {
    /// Nothing of its own: its children stand in its place.
    None,
    /// A box that holds others (a panel, a row of controls).
    Group,
    Button,
    /// A two-state control (WinUI's ToggleSwitch): on while the element is lit.
    Switch,
    /// A value in a range (`value`, `min`, `max`, `step`): the arrows step it.
    Slider,
    /// Items the arrows move between, one Tab stop for all of them.
    List,
    ListItem,
    TextInput,
    /// Text that names or tells something.
    Label,
    Heading,
    Image,
    Menu,
    MenuItem,
    Dialog,
}

impl Role {
    /// A `.nui` name (`role: list-item`).
    pub fn parse(s: &str) -> Option<Role> {
        Some(match s {
            "none" => Role::None,
            "group" => Role::Group,
            "button" => Role::Button,
            "switch" => Role::Switch,
            "slider" => Role::Slider,
            "list" => Role::List,
            "list-item" => Role::ListItem,
            "text-input" => Role::TextInput,
            "label" => Role::Label,
            "heading" => Role::Heading,
            "image" => Role::Image,
            "menu" => Role::Menu,
            "menu-item" => Role::MenuItem,
            "dialog" => Role::Dialog,
            _ => return None,
        })
    }
}

/// A slider's value and its range (`Role::Slider`).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Range {
    pub value: f64,
    pub min: f64,
    pub max: f64,
    /// What one arrow press moves it by.
    pub step: f64,
}

impl Default for Range {
    fn default() -> Self {
        Range { value: 0.0, min: 0.0, max: 1.0, step: 0.1 }
    }
}

impl Range {
    /// The value `steps` steps from the present one, kept in the range.
    pub fn stepped(&self, steps: f64) -> f64 {
        (self.value + steps * self.step).clamp(self.min.min(self.max), self.max.max(self.min))
    }
}

/// What is written of an element's semantics; `None` fields are inferred (`Tree::role`).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Semantics {
    pub role: Option<Role>,
    /// The name it is announced by, where its look alone does not say (an icon button).
    pub label: Option<String>,
    /// Whether Tab stops at it; inferred: it has an `on-click`, hears drags or is a text field.
    pub focusable: Option<bool>,
    pub range: Option<Range>,
    /// Its name is announced whenever it changes (a live region: the song now playing).
    pub live: bool,
    /// One of a set of which one is chosen (a dropdown's rows, a menu's choices): whether it is
    /// the one. A menu item with it is announced as a radio item, checked or not; its check mark
    /// is only its look.
    pub checked: Option<bool>,
}
