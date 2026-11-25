//! Browser actions and keyboard shortcuts (customizable per profile).

use std::collections::BTreeMap;

use winit::keyboard::{Key, ModifiersState, NamedKey};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Action {
    NewTab,
    CloseTab,
    ReopenClosedTab,
    NextTab,
    PrevTab,
    NewWindow,
    NewGuestWindow,
    FocusOmnibox,
    Reload,
    HardReload,
    Stop,
    Back,
    Forward,
    Home,
    Find,
    BookmarkPage,
    ToggleBookmarksBar,
    ShowBookmarks,
    ShowHistory,
    ShowDownloads,
    ShowExtensions,
    ShowSettings,
    ToggleAssistant,
    CommandPalette,
    ZoomIn,
    ZoomOut,
    ZoomReset,
    Fullscreen,
    ToggleVerticalTabs,
    DuplicateTab,
    PinTab,
    ClearBrowsingData,
    DevTools,
    Screenshot,
    SelectTab(u8),
    SelectLastTab,
    Quit,
    ProfileManager,
    About,
    ViewSource,
}

impl Action {
    /// Stable id used in settings.json keybinding overrides.
    pub fn id(&self) -> String {
        match self {
            Action::SelectTab(n) => format!("select_tab_{n}"),
            other => {
                let dbg = format!("{other:?}");
                let mut out = String::new();
                for (i, c) in dbg.chars().enumerate() {
                    if c.is_uppercase() && i > 0 {
                        out.push('_');
                    }
                    out.push(c.to_ascii_lowercase());
                }
                out
            }
        }
    }

    pub fn label(&self) -> String {
        match self {
            Action::NewTab => "Nueva pestaña".into(),
            Action::CloseTab => "Cerrar pestaña".into(),
            Action::ReopenClosedTab => "Reabrir pestaña cerrada".into(),
            Action::NextTab => "Pestaña siguiente".into(),
            Action::PrevTab => "Pestaña anterior".into(),
            Action::NewWindow => "Nueva ventana".into(),
            Action::NewGuestWindow => "Nueva ventana de invitado".into(),
            Action::FocusOmnibox => "Ir a la barra de direcciones".into(),
            Action::Reload => "Recargar".into(),
            Action::HardReload => "Recargar sin caché".into(),
            Action::Stop => "Detener".into(),
            Action::Back => "Atrás".into(),
            Action::Forward => "Adelante".into(),
            Action::Home => "Página de inicio".into(),
            Action::Find => "Buscar en la página".into(),
            Action::BookmarkPage => "Añadir a marcadores".into(),
            Action::ToggleBookmarksBar => "Mostrar/ocultar barra de marcadores".into(),
            Action::ShowBookmarks => "Marcadores".into(),
            Action::ShowHistory => "Historial".into(),
            Action::ShowDownloads => "Descargas".into(),
            Action::ShowExtensions => "Extensiones".into(),
            Action::ShowSettings => "Ajustes".into(),
            Action::ToggleAssistant => "Asistente IA".into(),
            Action::CommandPalette => "Paleta de comandos".into(),
            Action::ZoomIn => "Acercar".into(),
            Action::ZoomOut => "Alejar".into(),
            Action::ZoomReset => "Restablecer zoom".into(),
            Action::Fullscreen => "Pantalla completa".into(),
            Action::ToggleVerticalTabs => "Pestañas verticales".into(),
            Action::DuplicateTab => "Duplicar pestaña".into(),
            Action::PinTab => "Fijar/desfijar pestaña".into(),
            Action::ClearBrowsingData => "Borrar datos de navegación".into(),
            Action::DevTools => "Herramientas de desarrollo".into(),
            Action::Screenshot => "Captura de pantalla de la página".into(),
            Action::SelectTab(n) => format!("Ir a la pestaña {n}"),
            Action::SelectLastTab => "Ir a la última pestaña".into(),
            Action::Quit => "Salir".into(),
            Action::ProfileManager => "Gestionar perfiles".into(),
            Action::About => "Acerca de FerroSurf".into(),
            Action::ViewSource => "Ver código fuente".into(),
        }
    }

    /// Actions listed in the command palette and the shortcuts editor.
    pub fn all() -> Vec<Action> {
        use Action::*;
        vec![
            NewTab,
            CloseTab,
            ReopenClosedTab,
            NextTab,
            PrevTab,
            NewWindow,
            NewGuestWindow,
            FocusOmnibox,
            Reload,
            HardReload,
            Stop,
            Back,
            Forward,
            Home,
            Find,
            BookmarkPage,
            ToggleBookmarksBar,
            ShowBookmarks,
            ShowHistory,
            ShowDownloads,
            ShowExtensions,
            ShowSettings,
            ToggleAssistant,
            CommandPalette,
            ZoomIn,
            ZoomOut,
            ZoomReset,
            Fullscreen,
            ToggleVerticalTabs,
            DuplicateTab,
            PinTab,
            ClearBrowsingData,
            DevTools,
            Screenshot,
            SelectLastTab,
            ProfileManager,
            About,
            ViewSource,
            Quit,
        ]
    }
}

/// A key combination like `Ctrl+Shift+T`.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct Accelerator {
    pub ctrl: bool,
    pub shift: bool,
    pub alt: bool,
    pub logo: bool,
    /// Lower-case key name: a character ("t", "1", "+") or a named key ("f5", "tab", "left").
    pub key: String,
}

impl Accelerator {
    pub fn parse(s: &str) -> Option<Self> {
        let mut acc = Accelerator {
            ctrl: false,
            shift: false,
            alt: false,
            logo: false,
            key: String::new(),
        };
        // Allow "Ctrl++" (plus key).
        let s = s.trim();
        let (mods, key) = if let Some(stripped) = s.strip_suffix("++") {
            (stripped, "+")
        } else {
            match s.rsplit_once('+') {
                Some((m, k)) => (m, k),
                None => ("", s),
            }
        };
        for m in mods.split('+').filter(|m| !m.is_empty()) {
            match m.trim().to_ascii_lowercase().as_str() {
                "ctrl" | "control" | "cmd" | "cmdorctrl" => acc.ctrl = true,
                "shift" => acc.shift = true,
                "alt" | "option" => acc.alt = true,
                "super" | "meta" | "win" | "logo" => acc.logo = true,
                _ => return None,
            }
        }
        let key = key.trim().to_ascii_lowercase();
        if key.is_empty() {
            return None;
        }
        acc.key = key;
        Some(acc)
    }

    pub fn display(&self) -> String {
        let mut parts = Vec::new();
        if self.ctrl {
            parts.push("Ctrl".to_string());
        }
        if self.shift {
            parts.push("Shift".to_string());
        }
        if self.alt {
            parts.push("Alt".to_string());
        }
        if self.logo {
            parts.push("Super".to_string());
        }
        let mut k = self.key.clone();
        if k.len() == 1 {
            k = k.to_uppercase();
        } else if let Some(first) = k.get(..1) {
            k = first.to_uppercase() + &k[1..];
        }
        parts.push(k);
        parts.join("+")
    }

    /// Build from a winit key press.
    pub fn from_key(key: &Key, mods: ModifiersState) -> Option<Self> {
        let name = match key {
            Key::Character(c) => c.to_lowercase(),
            Key::Named(n) => match n {
                NamedKey::Tab => "tab".into(),
                NamedKey::Escape => "escape".into(),
                NamedKey::Enter => "enter".into(),
                NamedKey::ArrowLeft => "left".into(),
                NamedKey::ArrowRight => "right".into(),
                NamedKey::ArrowUp => "up".into(),
                NamedKey::ArrowDown => "down".into(),
                NamedKey::PageUp => "pageup".into(),
                NamedKey::PageDown => "pagedown".into(),
                NamedKey::Home => "home".into(),
                NamedKey::End => "end".into(),
                NamedKey::Delete => "delete".into(),
                NamedKey::Backspace => "backspace".into(),
                NamedKey::BrowserBack => "browserback".into(),
                NamedKey::BrowserForward => "browserforward".into(),
                NamedKey::BrowserRefresh => "browserrefresh".into(),
                NamedKey::Space => " ".into(),
                other => {
                    let dbg = format!("{other:?}").to_ascii_lowercase();
                    if dbg.starts_with('f') && dbg[1..].parse::<u8>().is_ok() {
                        dbg
                    } else {
                        return None;
                    }
                }
            },
            _ => return None,
        };
        // Shifted punctuation: treat "=" with ctrl as "+" for zoom.
        let name = if name == "=" { "+".to_string() } else { name };
        Some(Accelerator {
            ctrl: mods.control_key(),
            shift: mods.shift_key(),
            alt: mods.alt_key(),
            logo: mods.super_key(),
            key: name,
        })
    }

    /// Matching that tolerates shift for symbols that need shift on some layouts ("+").
    fn matches(&self, other: &Accelerator) -> bool {
        if self.key != other.key
            || self.ctrl != other.ctrl
            || self.alt != other.alt
            || self.logo != other.logo
        {
            return false;
        }
        self.shift == other.shift
            || (!self.key.chars().all(|c| c.is_alphanumeric()) && self.key.len() == 1)
    }
}

pub fn default_bindings() -> Vec<(Action, &'static str)> {
    use Action::*;
    vec![
        (NewTab, "Ctrl+T"),
        (CloseTab, "Ctrl+W"),
        (CloseTab, "Ctrl+F4"),
        (ReopenClosedTab, "Ctrl+Shift+T"),
        (NextTab, "Ctrl+Tab"),
        (NextTab, "Ctrl+PageDown"),
        (PrevTab, "Ctrl+Shift+Tab"),
        (PrevTab, "Ctrl+PageUp"),
        (NewWindow, "Ctrl+N"),
        (NewGuestWindow, "Ctrl+Shift+N"),
        (FocusOmnibox, "Ctrl+L"),
        (FocusOmnibox, "Alt+D"),
        (FocusOmnibox, "F6"),
        (Reload, "Ctrl+R"),
        (Reload, "F5"),
        (Reload, "BrowserRefresh"),
        (HardReload, "Ctrl+Shift+R"),
        (HardReload, "Ctrl+F5"),
        (Stop, "Escape"),
        (Back, "Alt+Left"),
        (Back, "BrowserBack"),
        (Forward, "Alt+Right"),
        (Forward, "BrowserForward"),
        (Home, "Alt+Home"),
        (Find, "Ctrl+F"),
        (Find, "F3"),
        (BookmarkPage, "Ctrl+D"),
        (ToggleBookmarksBar, "Ctrl+Shift+B"),
        (ShowBookmarks, "Ctrl+Shift+O"),
        (ShowHistory, "Ctrl+H"),
        (ShowDownloads, "Ctrl+Shift+Y"),
        (ShowExtensions, "Ctrl+Shift+E"),
        (ShowSettings, "Ctrl+,"),
        (ToggleAssistant, "Ctrl+J"),
        (CommandPalette, "Ctrl+K"),
        (CommandPalette, "Ctrl+Shift+P"),
        (ZoomIn, "Ctrl++"),
        (ZoomOut, "Ctrl+-"),
        (ZoomReset, "Ctrl+0"),
        (Fullscreen, "F11"),
        (ToggleVerticalTabs, "Ctrl+Shift+V"),
        (DuplicateTab, "Ctrl+Shift+K"),
        (ClearBrowsingData, "Ctrl+Shift+Delete"),
        (DevTools, "F12"),
        (Screenshot, "Ctrl+Shift+S"),
        (SelectTab(1), "Ctrl+1"),
        (SelectTab(2), "Ctrl+2"),
        (SelectTab(3), "Ctrl+3"),
        (SelectTab(4), "Ctrl+4"),
        (SelectTab(5), "Ctrl+5"),
        (SelectTab(6), "Ctrl+6"),
        (SelectTab(7), "Ctrl+7"),
        (SelectTab(8), "Ctrl+8"),
        (SelectLastTab, "Ctrl+9"),
        (ViewSource, "Ctrl+U"),
        (Quit, "Ctrl+Q"),
    ]
}

/// Resolved keymap: defaults with user overrides applied.
pub struct Keymap {
    bindings: Vec<(Accelerator, Action)>,
}

impl Keymap {
    pub fn new(overrides: &BTreeMap<String, String>) -> Self {
        let mut bindings = Vec::new();
        let mut overridden = std::collections::HashSet::new();
        for action in Action::all()
            .into_iter()
            .chain((1..=8).map(Action::SelectTab))
        {
            if let Some(spec) = overrides.get(&action.id()) {
                overridden.insert(action);
                for s in spec.split(',') {
                    if let Some(acc) = Accelerator::parse(s) {
                        bindings.push((acc, action));
                    }
                }
            }
        }
        for (action, spec) in default_bindings() {
            if !overridden.contains(&action)
                && let Some(acc) = Accelerator::parse(spec)
            {
                bindings.push((acc, action));
            }
        }
        Self { bindings }
    }

    pub fn lookup(&self, acc: &Accelerator) -> Option<Action> {
        self.bindings
            .iter()
            .find(|(a, _)| a.matches(acc))
            .map(|(_, act)| *act)
    }

    pub fn shortcut_for(&self, action: Action) -> Option<String> {
        self.bindings
            .iter()
            .find(|(_, a)| *a == action)
            .map(|(acc, _)| acc.display())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_and_lookup() {
        let km = Keymap::new(&BTreeMap::new());
        let a = Accelerator::parse("Ctrl+Shift+T").unwrap();
        assert_eq!(km.lookup(&a), Some(Action::ReopenClosedTab));
        let plus = Accelerator {
            ctrl: true,
            shift: true,
            alt: false,
            logo: false,
            key: "+".into(),
        };
        assert_eq!(km.lookup(&plus), Some(Action::ZoomIn));
        assert_eq!(Accelerator::parse("Ctrl++").unwrap().key, "+");
        assert_eq!(km.shortcut_for(Action::NewTab).unwrap(), "Ctrl+T");
        assert_eq!(Action::ToggleBookmarksBar.id(), "toggle_bookmarks_bar");
    }

    #[test]
    fn overrides() {
        let mut o = BTreeMap::new();
        o.insert("new_tab".to_string(), "Ctrl+Alt+N".to_string());
        let km = Keymap::new(&o);
        assert_eq!(km.lookup(&Accelerator::parse("Ctrl+T").unwrap()), None);
        assert_eq!(
            km.lookup(&Accelerator::parse("Ctrl+Alt+N").unwrap()),
            Some(Action::NewTab)
        );
    }
}
