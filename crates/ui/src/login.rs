//! The sign-in screen: the first thing a person sees, and the only screen
//! here that exists before there is a world.
//!
//! Everything else this crate draws is a *second view of something already on
//! the wire*. This one is the opposite -- it is where the wire gets told what
//! to connect to -- and that inverts two of the crate's habits, so both are
//! stated here rather than discovered later:
//!
//! * **It is not an [`crate::element::Element`].** It has no anchor, no
//!   offset and no scale, because it is drawn centred on an empty window and
//!   the layout editor that would move it around cannot be opened until it has
//!   been dismissed. `login_width` and `login_row` in [`Style`] are the whole
//!   of its geometry.
//! * **It holds state.** [`crate::HudData`] is rebuilt from the world every
//!   frame precisely so nothing here can go stale; a half-typed password has
//!   no world to be rebuilt from, so it lives in [`SignIn`] and this module
//!   owns it.
//!
//! What it deliberately does **not** do is create characters. That is a
//! decision, not a gap: character creation is a race and class list, an
//! appearance picker driven by tables this client reads for other reasons, a
//! name-validity dialogue and a server verdict with a dozen refusal codes --
//! and every one of those refusals is a *string* nobody here has verified.
//! The original client does it correctly today. This one signs in.
//!
//! The password is **never written to disk**. Not as a setting, not
//! obfuscated, not behind a "remember me" -- see [`Settings`], which has no
//! field for it. Everything else about a sign-in is remembered, because
//! retyping a server address every launch is the thing that makes a login
//! screen worse than a command line.

use std::path::PathBuf;

use egui::{Align2, Color32, FontId, Painter, Pos2, Rect, Stroke, StrokeKind, Vec2};

use crate::style::{Color, Style};
use crate::theme::Theme;
use crate::Error;

/// The default logon port, repeated here rather than depended on: this crate
/// does not know the `auth` crate exists, and a sign-in screen that could not
/// suggest a port would be asking every user to know one.
pub const DEFAULT_PORT: u16 = 3724;

/// What is remembered between launches.
///
/// **No password field, and that is the design.** A client that stores one
/// stores it in plain text on a machine other people may use, and "obfuscated"
/// is plain text with an extra step. The account name is remembered, which is
/// the half that is tedious and not a secret.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Settings {
    /// The installation's `Data` directory. `None` until somebody has said,
    /// which is the state a fresh install starts in and the reason the
    /// settings panel opens itself.
    pub data: Option<PathBuf>,
    pub locale: String,
    /// Host, or `host:port`. One field rather than two because that is how
    /// people are given a realm -- and a port that is nearly always 3724 does
    /// not deserve a permanent box on the screen.
    pub server: String,
    pub account: String,
    /// The realm last entered, so a list of many preselects the right one.
    pub realm: Option<String>,
    /// The character last played, likewise.
    pub character: Option<String>,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            data: None,
            locale: "enUS".into(),
            server: String::new(),
            account: String::new(),
            realm: None,
            character: None,
        }
    }
}

impl Settings {
    /// `%APPDATA%\open-wow\login.toml`, beside the layout.
    ///
    /// **A separate file from `ui.toml`, deliberately.** That file is the
    /// interface's layout and is the thing a person hand-edits and shares;
    /// this one is a machine's memory of the last thing typed into a form.
    /// Merging them would mean a user swapping layouts also swapped which
    /// account they were signing in as.
    pub fn default_path() -> Result<PathBuf, Error> {
        let layout = crate::layout::default_path()?;
        Ok(layout.with_file_name("login.toml"))
    }

    /// Reads the file, or returns the defaults if it is not there.
    ///
    /// A missing file is not an error -- it is what a first run looks like --
    /// but a *malformed* one is, for the same reason the layout is strict:
    /// silently substituting defaults teaches the user the file is being read
    /// when it is not.
    pub fn load_from(path: &std::path::Path) -> Result<Settings, Error> {
        match std::fs::read_to_string(path) {
            Ok(text) => Ok(toml::from_str(&text)?),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Settings::default()),
            Err(e) => Err(e.into()),
        }
    }

    pub fn save_to(&self, path: &std::path::Path) -> Result<(), Error> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(path, toml::to_string_pretty(self)?)?;
        Ok(())
    }

    /// Splits [`Self::server`] into a host and a port.
    ///
    /// A trailing `:` or a port that is not a number falls back to
    /// [`DEFAULT_PORT`] rather than refusing: the overwhelmingly common input
    /// is a bare hostname, and someone halfway through typing `:80` should not
    /// see an error where the address is still being written.
    pub fn address(&self) -> (String, u16) {
        let server = self.server.trim();
        match server.rsplit_once(':') {
            Some((host, port)) => (
                host.trim().to_string(),
                port.trim().parse().unwrap_or(DEFAULT_PORT),
            ),
            None => (server.to_string(), DEFAULT_PORT),
        }
    }
}

/// One realm the logon server offered.
#[derive(Clone, Debug, PartialEq)]
pub struct RealmRow {
    pub name: String,
    /// What the row says underneath the name -- how busy it is, or that it is
    /// down. Composed by the caller, because what is worth saying about a
    /// realm is a fact about the `auth` crate's realm record and this crate
    /// has never heard of one.
    pub detail: String,
    /// Drawn dim and refused a click. A realm flagged offline is one the
    /// world connection will fail against, and letting it be picked spends a
    /// ten-second timeout to say so.
    pub offline: bool,
}

/// One character on the account, as the character-selection list shows it.
#[derive(Clone, Debug, PartialEq)]
pub struct CharacterRow {
    pub name: String,
    /// `12 Human Warrior -- Northshire Valley`, or as much of it as the
    /// caller could resolve. Composed by the caller for the same reason
    /// [`RealmRow::detail`] is: race, class and zone are three DBC lookups and
    /// this crate reads no tables.
    pub detail: String,
    /// A character the server will demand be renamed before it can be played.
    /// Drawn dim and refused, exactly like an offline realm: the alternative
    /// is entering the world and being thrown out with a code.
    pub blocked: bool,
}

/// Which panel is showing.
#[derive(Clone, Debug, PartialEq)]
pub enum Stage {
    /// Account, password and server.
    Credentials,
    /// Something is in flight and the screen can only wait. The string is
    /// what it is waiting for, shown as written: "signing in", "reading the
    /// character list". **A stage rather than a flag**, so there is no state
    /// in which the panel is both waiting and offering a button that would
    /// start a second attempt.
    Working(String),
    /// The logon server offered more than one realm.
    Realms(Vec<RealmRow>),
    Characters(Vec<CharacterRow>),
}

/// Why the status line is there.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Tone {
    /// Something to know: what was found, what to do next.
    Plain,
    /// Something went wrong. Drawn in [`Style::login_error`].
    Bad,
}

/// What the screen wants the caller to do.
///
/// Deliberately small and deliberately *verbs*: this crate opens no sockets,
/// reads no archives and touches no files but its own settings, so every
/// variant here is a thing the viewer does and reports back by changing
/// [`SignIn::stage`].
#[derive(Clone, Debug, PartialEq)]
pub enum Action {
    None,
    /// Sign in with the account, password and server as they currently stand.
    SignIn,
    /// The row at this index on whichever list stage is showing.
    Choose(usize),
    /// Back to the credentials panel, dropping whatever connection exists.
    Back,
    /// Close the window.
    Quit,
    /// Repaint the interface in this theme and write it to the layout file.
    Theme(Theme),
    /// Ask the operating system for a folder, because typing a Windows path
    /// by hand is a punishment. Optional: the field is editable, so a caller
    /// with no picker can ignore this and lose nothing.
    BrowseData,
    /// The data directory or the locale changed, so the archives have to be
    /// reopened. Reported rather than done here for the ordinary reason: this
    /// crate has never heard of an archive.
    DataChanged,
}

/// Which box has the keyboard.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Field {
    Account,
    Password,
    Server,
    Data,
    Locale,
}

/// The sign-in screen's own state.
pub struct SignIn {
    pub settings: Settings,
    /// **Not in [`Settings`], and never written anywhere.** See the module
    /// comment.
    pub password: String,
    pub stage: Stage,
    /// What the status line says, and whether it is a complaint.
    pub status: Option<(String, Tone)>,
    /// Whether the settings panel is showing instead of the sign-in one.
    pub settings_open: bool,
    /// Which row of a list is picked out. Kept across a stage change so the
    /// remembered character can be preselected -- see [`SignIn::show_list`].
    pub selected: usize,
    focus: Field,
    /// Where the settings live, resolved once so a save cannot fail
    /// differently from the load that preceded it.
    path: Option<PathBuf>,
}

impl SignIn {
    /// Reads the remembered settings and decides which panel opens.
    ///
    /// **A first run opens the settings panel, not the sign-in one.** With no
    /// data directory the sign-in button cannot do anything, and a screen
    /// whose only working control is an unlabelled gear is a screen people
    /// report as broken -- the same failure as trade's right-click, which was
    /// correct in every line and undiscoverable.
    pub fn new() -> Self {
        let path = Settings::default_path().ok();
        let (settings, status) = match &path {
            Some(path) => match Settings::load_from(path) {
                Ok(settings) => (settings, None),
                Err(e) => (
                    Settings::default(),
                    Some((format!("{path:?} could not be read: {e}"), Tone::Bad)),
                ),
            },
            None => (
                Settings::default(),
                Some((
                    "no configuration directory, so nothing will be remembered".into(),
                    Tone::Bad,
                )),
            ),
        };
        let fresh = settings.data.is_none();
        Self {
            focus: if settings.account.is_empty() {
                Field::Account
            } else {
                Field::Password
            },
            settings_open: fresh,
            status: status.or(fresh.then(|| {
                (
                    "point this at your WoW 3.3.5a Data folder to begin".into(),
                    Tone::Plain,
                )
            })),
            settings,
            password: String::new(),
            stage: Stage::Credentials,
            selected: 0,
            path,
        }
    }

    /// Writes the settings back, reporting a failure into the status line
    /// rather than to a caller who has nowhere to put it.
    pub fn save(&mut self) {
        let Some(path) = self.path.clone() else { return };
        if let Err(e) = self.settings.save_to(&path) {
            self.status = Some((format!("could not save {}: {e}", path.display()), Tone::Bad));
        }
    }

    /// Says what went wrong, and puts the panel back where it can be
    /// answered.
    ///
    /// **Always leaves [`Stage::Working`]**, because that stage draws no
    /// button: a failure that only set a message would leave the screen saying
    /// "signing in" forever with no way to try again.
    pub fn failed(&mut self, what: impl std::fmt::Display) {
        self.status = Some((what.to_string(), Tone::Bad));
        if matches!(self.stage, Stage::Working(_)) {
            self.stage = Stage::Credentials;
        }
    }

    pub fn working(&mut self, what: impl Into<String>) {
        self.stage = Stage::Working(what.into());
    }

    pub fn note(&mut self, what: impl Into<String>) {
        self.status = Some((what.into(), Tone::Plain));
    }

    /// Shows a realm list, preselecting the one last entered.
    pub fn show_realms(&mut self, realms: Vec<RealmRow>) {
        self.selected = self
            .settings
            .realm
            .as_deref()
            .and_then(|wanted| {
                realms
                    .iter()
                    .position(|r| r.name.eq_ignore_ascii_case(wanted))
            })
            .unwrap_or(0);
        self.stage = Stage::Realms(realms);
    }

    /// Shows a character list, preselecting the one last played.
    pub fn show_characters(&mut self, characters: Vec<CharacterRow>) {
        self.selected = self
            .settings
            .character
            .as_deref()
            .and_then(|wanted| {
                characters
                    .iter()
                    .position(|c| c.name.eq_ignore_ascii_case(wanted))
            })
            .unwrap_or(0);
        self.stage = Stage::Characters(characters);
    }

    /// Whether the sign-in button can do anything.
    ///
    /// Read by the drawing to grey the button out, which is the honest form
    /// of this check: a button that looks alive and silently does nothing is
    /// the failure this project has paid for in three other frames.
    pub fn ready(&self) -> bool {
        self.settings.data.is_some()
            && !self.settings.server.trim().is_empty()
            && !self.settings.account.trim().is_empty()
            && !self.password.is_empty()
    }

    fn field_mut(&mut self, field: Field) -> &mut String {
        match field {
            Field::Account => &mut self.settings.account,
            Field::Password => &mut self.password,
            Field::Server => &mut self.settings.server,
            Field::Locale => &mut self.settings.locale,
            // The data directory is a path everywhere else and a string only
            // while it is being typed, so it is edited through a scratch
            // buffer this function cannot hand out. See `type_into`.
            Field::Data => unreachable!("the data field is edited through its own path"),
        }
    }

    fn field_text(&self, field: Field) -> String {
        match field {
            Field::Account => self.settings.account.clone(),
            Field::Password => self.password.clone(),
            Field::Server => self.settings.server.clone(),
            Field::Locale => self.settings.locale.clone(),
            Field::Data => self
                .settings
                .data
                .as_ref()
                .map(|p| p.display().to_string())
                .unwrap_or_default(),
        }
    }

    /// The fields the panel currently showing can move between, in order.
    fn tab_ring(&self) -> &'static [Field] {
        if self.settings_open {
            &[Field::Data, Field::Locale]
        } else {
            &[Field::Account, Field::Password, Field::Server]
        }
    }
}

impl Default for SignIn {
    fn default() -> Self {
        Self::new()
    }
}

/// One drawn thing, with the rectangle it occupies.
///
/// **This is the geometry, stated once.** The drawing walks this list and so
/// does the hit test, which is the rule the party invite's two-button prompt
/// produced: two separately computed rectangles a few pixels apart leave a
/// press between them answering nothing.
enum Item {
    /// A caption above a field, or a line of ordinary text.
    Label { rect: Rect, text: String, dim: bool },
    Field {
        rect: Rect,
        field: Field,
        text: String,
        /// Drawn as bullets. The password, and nothing else.
        secret: bool,
        /// Shown in place of an empty field.
        hint: &'static str,
    },
    Button {
        rect: Rect,
        target: Target,
        text: String,
        /// Filled in the accent colour rather than outlined: the one thing
        /// this panel is for.
        primary: bool,
        enabled: bool,
    },
    /// One realm or character.
    Row {
        rect: Rect,
        index: usize,
        name: String,
        detail: String,
        enabled: bool,
    },
    /// Small print. Drawn dim, never clickable, and load-bearing exactly
    /// once: it is where the panel says character creation lives elsewhere.
    Note { rect: Rect, text: String },
}

/// What clicking an item means.
#[derive(Clone, Copy, Debug, PartialEq)]
enum Target {
    Focus(Field),
    SignIn,
    OpenSettings,
    CloseSettings,
    Browse,
    Theme(Theme),
    Choose,
    Back,
    Quit,
}

impl Item {
    fn rect(&self) -> Rect {
        match self {
            Item::Label { rect, .. }
            | Item::Field { rect, .. }
            | Item::Button { rect, .. }
            | Item::Row { rect, .. }
            | Item::Note { rect, .. } => *rect,
        }
    }

    /// What a click here does, or `None` for something that is only text.
    ///
    /// A row that cannot be entered and a button that cannot act both answer
    /// `None`, so the hit test declines them the same way it declines a
    /// caption -- the rule `row_at` follows in every list frame here: **answer
    /// only for rows that can act**, because the alternative is a request the
    /// server refuses in silence.
    fn target(&self) -> Option<Target> {
        match self {
            Item::Field { field, .. } => Some(Target::Focus(*field)),
            Item::Button { target, enabled, .. } => enabled.then_some(*target),
            Item::Row { enabled, .. } => enabled.then_some(Target::Choose),
            Item::Label { .. } | Item::Note { .. } => None,
        }
    }
}

/// The whole panel: what to draw, and how big it came out.
struct Panel {
    items: Vec<Item>,
    size: Vec2,
}

/// Builds the panel top to bottom, accumulating height as it goes.
struct Build {
    items: Vec<Item>,
    width: f32,
    row: f32,
    pad: f32,
    gap: f32,
    y: f32,
}

impl Build {
    fn new(style: &Style) -> Self {
        Self {
            items: Vec::new(),
            width: style.login_width,
            row: style.login_row,
            pad: style.padding * 2.0,
            gap: style.gap,
            y: 0.0,
        }
    }

    /// Reserves `height` across the full inner width and advances.
    fn take(&mut self, height: f32) -> Rect {
        let rect = Rect::from_min_size(
            Pos2::new(self.pad, self.pad + self.y),
            Vec2::new(self.width - self.pad * 2.0, height),
        );
        self.y += height + self.gap;
        rect
    }

    fn label(&mut self, text: impl Into<String>, dim: bool) {
        let rect = self.take(self.row * 0.62);
        self.items.push(Item::Label {
            rect,
            text: text.into(),
            dim,
        });
    }

    fn note(&mut self, text: impl Into<String>) {
        let text = text.into();
        // Two short lines rather than one long one: this panel is 360 points
        // wide and the note that matters most ("characters are made in the
        // original client") does not fit on one.
        let lines = text.matches('\n').count() as f32 + 1.0;
        let rect = self.take(self.row * 0.58 * lines);
        self.items.push(Item::Note { rect, text });
    }

    fn field(&mut self, field: Field, text: String, secret: bool, hint: &'static str) {
        let rect = self.take(self.row);
        self.items.push(Item::Field {
            rect,
            field,
            text,
            secret,
            hint,
        });
    }

    fn row(&mut self, index: usize, name: String, detail: String, enabled: bool) {
        let rect = self.take(self.row * 1.5);
        self.items.push(Item::Row {
            rect,
            index,
            name,
            detail,
            enabled,
        });
    }

    /// A line of buttons sharing the row, `weights` wide in proportion.
    fn buttons(&mut self, buttons: &[(Target, String, bool, bool)]) {
        let rect = self.take(self.row);
        let gap = self.gap;
        let total = rect.width() - gap * (buttons.len() as f32 - 1.0);
        let each = total / buttons.len() as f32;
        for (i, (target, text, primary, enabled)) in buttons.iter().enumerate() {
            let x = rect.left() + (each + gap) * i as f32;
            self.items.push(Item::Button {
                rect: Rect::from_min_size(Pos2::new(x, rect.top()), Vec2::new(each, rect.height())),
                target: *target,
                text: text.clone(),
                primary: *primary,
                enabled: *enabled,
            });
        }
    }

    /// A wide button with a narrow one beside it -- sign in, and the gear.
    fn button_and_gear(&mut self, main: (Target, String, bool, bool), gear: Target) {
        let rect = self.take(self.row);
        let gear_width = self.row;
        let main_width = rect.width() - gear_width - self.gap;
        self.items.push(Item::Button {
            rect: Rect::from_min_size(rect.min, Vec2::new(main_width, rect.height())),
            target: main.0,
            text: main.1,
            primary: main.2,
            enabled: main.3,
        });
        self.items.push(Item::Button {
            // A cat's head, not a cog. The client is called MeoWoW and this is
            // the one control on the screen with no room for a word.
            rect: Rect::from_min_size(
                Pos2::new(rect.right() - gear_width, rect.top()),
                Vec2::splat(rect.height()),
            ),
            target: gear,
            text: "\u{1f431}".into(),
            primary: false,
            enabled: true,
        });
    }

    /// The panel, and how tall it came out.
    ///
    /// The height falls out of the items rather than being declared: every
    /// stage here has a different number of rows on it, and a fixed height
    /// would leave the shortest panel with a lake of empty backing under it
    /// and clip the longest.
    fn finish(self) -> Panel {
        Panel {
            size: Vec2::new(self.width, self.y - self.gap + self.pad * 2.0),
            items: self.items,
        }
    }
}

/// How many rows of a list are shown before it becomes a window onto a longer
/// one.
///
/// **Nine, and the panel says so when it clips.** An account with fifty
/// characters is ordinary on a server that has been running for years, and a
/// list that silently showed the first nine would be the auction window's
/// mistake in a different frame: the rows are real, and the person reading
/// them believes that is all of them.
const VISIBLE_ROWS: usize = 9;

impl SignIn {
    /// Describes the panel for the current stage.
    fn panel(&self, style: &Style) -> Panel {
        let mut b = Build::new(style);
        if self.settings_open {
            self.build_settings(&mut b, style);
        } else {
            match &self.stage {
                Stage::Credentials => self.build_credentials(&mut b),
                Stage::Working(what) => {
                    b.label(what.clone(), false);
                    b.buttons(&[(Target::Back, "Cancel".into(), false, true)]);
                }
                Stage::Realms(realms) => {
                    b.label("Choose a realm", false);
                    self.build_rows(
                        &mut b,
                        realms.len(),
                        |i| (realms[i].name.clone(), realms[i].detail.clone()),
                        |i| !realms[i].offline,
                    );
                    b.buttons(&[
                        (
                            Target::Choose,
                            "Enter".into(),
                            true,
                            realms.get(self.selected).is_some_and(|r| !r.offline),
                        ),
                        (Target::Back, "Back".into(), false, true),
                    ]);
                }
                Stage::Characters(characters) => {
                    b.label("Choose a character", false);
                    if characters.is_empty() {
                        // The one state where the answer is elsewhere, said
                        // plainly rather than as an empty box.
                        b.note(
                            "This account has no characters on this realm.\n\
                             Make one in the original client, then come back.",
                        );
                    }
                    self.build_rows(
                        &mut b,
                        characters.len(),
                        |i| (characters[i].name.clone(), characters[i].detail.clone()),
                        |i| !characters[i].blocked,
                    );
                    b.buttons(&[
                        (
                            Target::Choose,
                            "Enter World".into(),
                            true,
                            characters.get(self.selected).is_some_and(|c| !c.blocked),
                        ),
                        (Target::Back, "Back".into(), false, true),
                    ]);
                    b.note("New characters are made in the original client.");
                }
            }
        }
        if let Some((text, tone)) = &self.status {
            b.label(text.clone(), *tone == Tone::Plain);
        }
        b.finish()
    }

    fn build_credentials(&self, b: &mut Build) {
        b.label("Account", true);
        b.field(Field::Account, self.settings.account.clone(), false, "");
        b.label("Password", true);
        b.field(Field::Password, self.password.clone(), true, "");
        b.label("Realm server", true);
        b.field(
            Field::Server,
            self.settings.server.clone(),
            false,
            "host or host:port",
        );
        b.button_and_gear(
            (Target::SignIn, "Sign In".into(), true, self.ready()),
            Target::OpenSettings,
        );
        b.buttons(&[(Target::Quit, "Quit".into(), false, true)]);
    }

    fn build_settings(&self, b: &mut Build, style: &Style) {
        b.label("Data directory", true);
        b.field(
            Field::Data,
            self.field_text(Field::Data),
            false,
            "the Data folder of a WoW 3.3.5a install",
        );
        b.buttons(&[(Target::Browse, "Browse\u{2026}".into(), false, true)]);
        b.label("Locale", true);
        b.field(Field::Locale, self.settings.locale.clone(), false, "enUS");
        b.label("Theme", true);
        let current = Theme::of(&style.clone());
        // Four buttons across one row. The selected one is filled, which is
        // the same "primary" treatment the sign-in button gets -- and when the
        // style has been hand-edited none of them is filled, because `of`
        // answers `None` and saying otherwise would be a claim nobody could
        // check.
        let themes: Vec<(Target, String, bool, bool)> = Theme::ALL
            .into_iter()
            .map(|t| {
                (
                    Target::Theme(t),
                    t.name().to_string(),
                    current == Some(t),
                    true,
                )
            })
            .collect();
        b.buttons(&themes);
        b.buttons(&[(Target::CloseSettings, "Done".into(), true, true)]);
    }

    /// The visible window onto a list, and the line that says it is one.
    fn build_rows(
        &self,
        b: &mut Build,
        count: usize,
        row: impl Fn(usize) -> (String, String),
        enabled: impl Fn(usize) -> bool,
    ) {
        let first = self.selected.saturating_sub(VISIBLE_ROWS - 1).min(
            count.saturating_sub(VISIBLE_ROWS),
        );
        for i in first..count.min(first + VISIBLE_ROWS) {
            let (name, detail) = row(i);
            b.row(i, name, detail, enabled(i));
        }
        if count > VISIBLE_ROWS {
            b.note(format!(
                "{}-{} of {count}",
                first + 1,
                (first + VISIBLE_ROWS).min(count)
            ));
        }
    }

    /// Draws the screen and returns what the person did.
    ///
    /// Takes the whole context rather than a painter and a rectangle, unlike
    /// every frame in [`crate::frames`], because this screen *is* the window
    /// while it is up: it owns the keyboard, it centres itself on the
    /// viewport, and there is no layout to have placed it.
    pub fn show(&mut self, ctx: &egui::Context, style: &Style) -> Action {
        let action = self.read_keyboard(ctx);
        let screen = ctx.content_rect();
        let panel = self.panel(style);
        let origin = Pos2::new(
            (screen.center().x - panel.size.x / 2.0).round(),
            (screen.center().y - panel.size.y / 2.0).round().max(8.0),
        );

        let layer = egui::LayerId::new(egui::Order::Foreground, egui::Id::new("sign-in"));
        let response = egui::Area::new(layer.id)
            .order(egui::Order::Foreground)
            .fixed_pos(origin)
            .show(ctx, |ui| {
                let (response, painter) = ui.allocate_painter(panel.size, egui::Sense::click());
                self.draw(&painter, response.rect, &panel, style);
                response
            })
            .inner;

        if action != Action::None {
            return action;
        }
        // A press anywhere on the panel, resolved through the same list the
        // drawing used.
        let Some(pos) = response.interact_pointer_pos() else {
            return Action::None;
        };
        if !response.clicked() {
            return Action::None;
        }
        let local = pos - response.rect.min.to_vec2();
        let hit = panel
            .items
            .iter()
            .find(|item| item.rect().contains(local))
            .and_then(|item| item.target().map(|t| (t, item)));
        let Some((target, item)) = hit else {
            return Action::None;
        };
        if let Item::Row { index, .. } = item {
            self.selected = *index;
        }
        self.act(target)
    }

    /// Turns one press into an action, and applies the ones that are purely
    /// this screen's own business.
    fn act(&mut self, target: Target) -> Action {
        match target {
            Target::Focus(field) => {
                self.focus = field;
                Action::None
            }
            Target::SignIn => Action::SignIn,
            Target::OpenSettings => {
                self.settings_open = true;
                self.focus = Field::Data;
                Action::None
            }
            Target::CloseSettings => {
                self.settings_open = false;
                self.focus = if self.settings.account.is_empty() {
                    Field::Account
                } else {
                    Field::Password
                };
                self.save();
                Action::DataChanged
            }
            Target::Browse => Action::BrowseData,
            Target::Theme(theme) => Action::Theme(theme),
            Target::Choose => Action::Choose(self.selected),
            Target::Back => Action::Back,
            Target::Quit => Action::Quit,
        }
    }

    /// Typing, tabbing, arrowing and pasting.
    ///
    /// Read straight off egui's event queue rather than routed in from winit,
    /// which is the substrate doing the one job it is here for. `Event::Text`
    /// is what knows about layouts, shift and dead keys -- deriving a
    /// character from a physical key code types QWERTY on an AZERTY keyboard,
    /// which is the same reason `type_into_chat` takes the text and not the
    /// code.
    fn read_keyboard(&mut self, ctx: &egui::Context) -> Action {
        let events = ctx.input(|i| i.events.clone());
        let mut action = Action::None;
        for event in events {
            match event {
                egui::Event::Text(text) => self.type_into(&text),
                egui::Event::Paste(text) => self.type_into(&text),
                egui::Event::Key {
                    key,
                    pressed: true,
                    modifiers,
                    ..
                } => {
                    if let Some(next) = self.key(key, modifiers) {
                        action = next;
                    }
                }
                _ => {}
            }
        }
        action
    }

    fn key(&mut self, key: egui::Key, modifiers: egui::Modifiers) -> Option<Action> {
        match key {
            egui::Key::Backspace => {
                self.backspace();
                None
            }
            egui::Key::Tab => {
                let ring = self.tab_ring();
                let at = ring.iter().position(|f| *f == self.focus).unwrap_or(0);
                let next = if modifiers.shift {
                    (at + ring.len() - 1) % ring.len()
                } else {
                    (at + 1) % ring.len()
                };
                self.focus = ring[next];
                None
            }
            egui::Key::Enter => Some(self.submit()),
            egui::Key::Escape => Some(if self.settings_open {
                self.settings_open = false;
                self.save();
                Action::DataChanged
            } else {
                match self.stage {
                    // **Escape does not quit here**, though every game this
                    // one resembles lets it. There is a Quit button two rows
                    // down, and the one thing on this panel that cannot be
                    // recovered by pressing something again is a password
                    // typed and then thrown away by a key somebody hit out of
                    // habit while a field had the keyboard.
                    Stage::Credentials => Action::None,
                    _ => Action::Back,
                }
            }),
            egui::Key::ArrowDown => {
                self.move_selection(1);
                None
            }
            egui::Key::ArrowUp => {
                self.move_selection(-1);
                None
            }
            _ => None,
        }
    }

    /// What Enter means, which depends entirely on which panel is up.
    fn submit(&mut self) -> Action {
        if self.settings_open {
            self.settings_open = false;
            self.save();
            return Action::DataChanged;
        }
        match &self.stage {
            Stage::Credentials => {
                // Enter in the account box moves on rather than submitting a
                // form with an empty password: the alternative spends a round
                // trip to be told what the screen already knew.
                if self.focus == Field::Account && self.password.is_empty() {
                    self.focus = Field::Password;
                    return Action::None;
                }
                if self.ready() {
                    Action::SignIn
                } else {
                    Action::None
                }
            }
            Stage::Working(_) => Action::None,
            Stage::Realms(realms) => realms
                .get(self.selected)
                .filter(|r| !r.offline)
                .map(|_| Action::Choose(self.selected))
                .unwrap_or(Action::None),
            Stage::Characters(characters) => characters
                .get(self.selected)
                .filter(|c| !c.blocked)
                .map(|_| Action::Choose(self.selected))
                .unwrap_or(Action::None),
        }
    }

    fn move_selection(&mut self, by: i32) {
        let count = match &self.stage {
            Stage::Realms(realms) => realms.len(),
            Stage::Characters(characters) => characters.len(),
            _ => return,
        };
        if count == 0 {
            return;
        }
        let at = self.selected.min(count - 1) as i32 + by;
        self.selected = at.clamp(0, count as i32 - 1) as usize;
    }

    fn type_into(&mut self, text: &str) {
        // Control characters arrive as text too -- Enter as "\r", Escape as
        // an escape -- and appending one puts an unprintable glyph in an
        // account name. The same filter `type_into_chat` applies.
        let typed: String = text.chars().filter(|c| !c.is_control()).collect();
        if typed.is_empty() {
            return;
        }
        match self.focus {
            Field::Data => {
                let mut path = self.field_text(Field::Data);
                path.push_str(&typed);
                self.settings.data = Some(PathBuf::from(path));
            }
            field => self.field_mut(field).push_str(&typed),
        }
    }

    fn backspace(&mut self) {
        match self.focus {
            Field::Data => {
                let mut path = self.field_text(Field::Data);
                path.pop();
                // An emptied box is "not set", not a path to the current
                // directory -- which is what `PathBuf::from("")` is, and it
                // opens no archives while looking like a setting.
                self.settings.data = (!path.is_empty()).then(|| PathBuf::from(path));
            }
            field => {
                self.field_mut(field).pop();
            }
        }
    }

    fn draw(&self, painter: &Painter, rect: Rect, panel: &Panel, style: &Style) {
        let corner = corner_radius(style.corner * 2.0);
        painter.rect_filled(rect, corner, style.login_background);
        painter.rect_stroke(
            rect,
            corner,
            Stroke::new(style.border_width.max(1.0), style.login_accent),
            StrokeKind::Inside,
        );
        draw_ears(painter, rect, style);

        let font = FontId::proportional(style.font_size);
        let small = FontId::proportional(style.font_size * 0.85);
        let error = self.status.as_ref().is_some_and(|(_, t)| *t == Tone::Bad);

        for item in &panel.items {
            let at = item.rect().translate(rect.min.to_vec2());
            match item {
                Item::Label { text, dim, .. } => {
                    // The status line is the last label and is the one that
                    // may be a complaint, so it is the only one that can take
                    // the error colour.
                    let colour = if !dim && error && std::ptr::eq(item, panel.items.last().unwrap())
                    {
                        style.login_error
                    } else if *dim {
                        style.quest_dim
                    } else {
                        style.text
                    };
                    painter.text(
                        at.left_center(),
                        Align2::LEFT_CENTER,
                        text,
                        if *dim { small.clone() } else { font.clone() },
                        colour.into(),
                    );
                }
                Item::Note { text, .. } => {
                    for (i, line) in text.lines().enumerate() {
                        painter.text(
                            at.left_top() + Vec2::new(0.0, small.size * 1.25 * i as f32),
                            Align2::LEFT_TOP,
                            line,
                            small.clone(),
                            style.quest_dim.into(),
                        );
                    }
                }
                Item::Field {
                    field,
                    text,
                    secret,
                    hint,
                    ..
                } => {
                    let focused = self.focus == *field;
                    painter.rect_filled(at, corner_radius(style.corner), style.login_field);
                    painter.rect_stroke(
                        at,
                        corner_radius(style.corner),
                        Stroke::new(
                            if focused { 2.0 } else { style.border_width.max(1.0) },
                            if focused {
                                Color32::from(style.login_accent)
                            } else {
                                Color32::from(style.border)
                            },
                        ),
                        StrokeKind::Inside,
                    );
                    let inner = at.shrink2(Vec2::new(style.padding, 0.0));
                    let clipped = painter.with_clip_rect(inner);
                    let shown = if *secret {
                        "\u{2022}".repeat(text.chars().count())
                    } else {
                        text.clone()
                    };
                    if shown.is_empty() && !hint.is_empty() {
                        clipped.text(
                            inner.left_center(),
                            Align2::LEFT_CENTER,
                            hint,
                            small.clone(),
                            style.quest_dim.into(),
                        );
                    }
                    // Right-aligned once it overflows, so the *end* of a long
                    // path stays visible: the tail is what a person is typing
                    // and the head is what they already got right.
                    let width = clipped
                        .layout_no_wrap(shown.clone(), font.clone(), style.text.into())
                        .size()
                        .x;
                    let anchor = if width > inner.width() {
                        (inner.right_center(), Align2::RIGHT_CENTER)
                    } else {
                        (inner.left_center(), Align2::LEFT_CENTER)
                    };
                    let text_rect =
                        clipped.text(anchor.0, anchor.1, &shown, font.clone(), style.text.into());
                    if focused {
                        let x = text_rect.right().min(inner.right()) + 1.0;
                        clipped.line_segment(
                            [
                                Pos2::new(x, inner.top() + 4.0),
                                Pos2::new(x, inner.bottom() - 4.0),
                            ],
                            Stroke::new(1.5, Color32::from(style.login_accent)),
                        );
                    }
                }
                Item::Button {
                    text,
                    primary,
                    enabled,
                    ..
                } => {
                    let accent: Color32 = style.login_accent.into();
                    if *primary && *enabled {
                        painter.rect_filled(at, corner_radius(style.corner), accent);
                    } else {
                        painter.rect_filled(at, corner_radius(style.corner), style.login_field);
                    }
                    painter.rect_stroke(
                        at,
                        corner_radius(style.corner),
                        Stroke::new(
                            style.border_width.max(1.0),
                            if *enabled {
                                accent
                            } else {
                                Color32::from(style.border)
                            },
                        ),
                        StrokeKind::Inside,
                    );
                    let colour = if !*enabled {
                        style.quest_dim.into()
                    } else if *primary {
                        // Against a filled accent, so it is picked to contrast
                        // with that rather than with the panel.
                        contrasting(style.login_accent)
                    } else {
                        style.text.into()
                    };
                    painter.text(at.center(), Align2::CENTER_CENTER, text, font.clone(), colour);
                }
                Item::Row {
                    index,
                    name,
                    detail,
                    enabled,
                    ..
                } => {
                    let picked = *index == self.selected;
                    if picked {
                        painter.rect_filled(
                            at,
                            corner_radius(style.corner),
                            style.spellbook_selected,
                        );
                    }
                    painter.rect_stroke(
                        at,
                        corner_radius(style.corner),
                        Stroke::new(
                            style.border_width.max(1.0),
                            if picked {
                                Color32::from(style.login_accent)
                            } else {
                                Color32::from(style.border)
                            },
                        ),
                        StrokeKind::Inside,
                    );
                    let inner = at.shrink2(Vec2::new(style.padding, style.gap * 0.5));
                    let clipped = painter.with_clip_rect(inner);
                    let name_colour = if *enabled { style.text } else { style.quest_dim };
                    clipped.text(
                        inner.left_top(),
                        Align2::LEFT_TOP,
                        name,
                        font.clone(),
                        name_colour.into(),
                    );
                    clipped.text(
                        inner.left_bottom(),
                        Align2::LEFT_BOTTOM,
                        detail,
                        small.clone(),
                        style.quest_dim.into(),
                    );
                }
            }
        }
    }
}

/// Two ears and a set of whiskers on the panel's top edge.
///
/// The whole of the client's identity on this screen, and it is drawn rather
/// than shipped: this project commits no art, and a cat made of four triangles
/// and six lines scales with `Style` the way every other frame here does.
fn draw_ears(painter: &Painter, rect: Rect, style: &Style) {
    let accent: Color32 = style.login_accent.into();
    let inner: Color32 = style.login_background.into();
    let h = style.login_row * 0.9;
    let w = h * 0.85;
    for side in [-1.0f32, 1.0] {
        let base = Pos2::new(rect.center().x + side * (style.login_width * 0.22), rect.top());
        let tip = Pos2::new(base.x + side * w * 0.25, base.y - h);
        let left = Pos2::new(base.x - w * 0.5, base.y);
        let right = Pos2::new(base.x + w * 0.5, base.y);
        painter.add(egui::Shape::convex_polygon(
            vec![left, tip, right],
            accent,
            Stroke::NONE,
        ));
        // The pink inside, drawn as a smaller triangle sharing the tip.
        painter.add(egui::Shape::convex_polygon(
            vec![
                Pos2::new(base.x - w * 0.24, base.y - 1.0),
                Pos2::new(tip.x, tip.y + h * 0.42),
                Pos2::new(base.x + w * 0.24, base.y - 1.0),
            ],
            inner,
            Stroke::NONE,
        ));
    }
    // Whiskers, three a side, from just inside each edge of the panel.
    let stroke = Stroke::new(1.0, accent.gamma_multiply(0.55));
    for side in [-1.0f32, 1.0] {
        let x = rect.center().x + side * style.login_width * 0.5;
        for (i, dy) in [-4.0f32, 0.0, 4.0].into_iter().enumerate() {
            let length = style.login_row * (1.1 - i as f32 * 0.12);
            let y = rect.top() + style.login_row * 0.75 + dy;
            painter.line_segment(
                [
                    Pos2::new(x - side * 2.0, y),
                    Pos2::new(x - side * (2.0 + length), y + dy * 0.4),
                ],
                stroke,
            );
        }
    }
}

/// Black or white, whichever is readable on `background`.
///
/// Needed because the accent is a theme's choice and runs from a pale pink to
/// a mid violet: a text colour fixed at either end is unreadable on one of
/// them, and the button it sits on is the one control this screen exists for.
fn contrasting(background: Color) -> Color32 {
    let [r, g, b, _] = background.0;
    // Rec. 601 luma, which is the cheap approximation everybody uses for
    // exactly this decision.
    let luma = 0.299 * r as f32 + 0.587 * g as f32 + 0.114 * b as f32;
    if luma > 140.0 {
        Color32::from_rgb(16, 14, 20)
    } else {
        Color32::from_rgb(250, 248, 252)
    }
}

fn corner_radius(radius: f32) -> egui::CornerRadius {
    egui::CornerRadius::same(radius.round().clamp(0.0, 255.0) as u8)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn screen() -> SignIn {
        SignIn {
            settings: Settings {
                data: Some(PathBuf::from("D:/Games/WoW/Data")),
                locale: "enUS".into(),
                server: "127.0.0.1".into(),
                account: "OWC33".into(),
                realm: None,
                character: None,
            },
            password: "hunter2".into(),
            stage: Stage::Credentials,
            status: None,
            settings_open: false,
            selected: 0,
            focus: Field::Password,
            // No file, so nothing a test does can reach the real settings.
            path: None,
        }
    }

    #[test]
    fn a_bare_host_gets_the_default_port() {
        let mut settings = Settings::default();
        settings.server = "wow1.nekos.farm".into();
        assert_eq!(
            settings.address(),
            ("wow1.nekos.farm".into(), DEFAULT_PORT)
        );
        settings.server = "127.0.0.1:3725".into();
        assert_eq!(settings.address(), ("127.0.0.1".into(), 3725));
        // Half-typed, and it must not error: the box is still being written.
        settings.server = "127.0.0.1:".into();
        assert_eq!(settings.address(), ("127.0.0.1".into(), DEFAULT_PORT));
    }

    /// The settings file is the one thing this screen writes, and everything
    /// it remembers has to survive it -- a server address that reverted every
    /// launch is exactly the tedium a login screen is supposed to remove.
    #[test]
    fn the_settings_survive_the_file() {
        let settings = screen().settings;
        let text = toml::to_string_pretty(&settings).expect("serialise");
        let parsed: Settings = toml::from_str(&text).expect("parse");
        assert_eq!(parsed, settings);
    }

    /// **The password must not be in the file.** Asserted on the text rather
    /// than on the struct, because the struct not having the field is what a
    /// reviewer sees and the serialised bytes are what a person's disk gets.
    #[test]
    fn the_file_cannot_hold_a_password() {
        let screen = screen();
        let text = toml::to_string_pretty(&screen.settings).expect("serialise");
        assert!(!text.contains("hunter2"), "{text}");
        assert!(!text.to_lowercase().contains("password"), "{text}");
    }

    #[test]
    fn signing_in_needs_every_part() {
        let mut screen = screen();
        assert!(screen.ready());
        screen.password.clear();
        assert!(!screen.ready());
        screen.password = "x".into();
        screen.settings.data = None;
        assert!(!screen.ready());
        screen.settings.data = Some(PathBuf::from("D:/"));
        screen.settings.server = "  ".into();
        assert!(!screen.ready());
    }

    /// Typing goes to whatever has the keyboard, and the data directory is
    /// the field that is a `PathBuf` everywhere except while it is being
    /// typed -- the one place an off-by-one would send characters nowhere.
    #[test]
    fn typing_reaches_every_field() {
        let mut screen = screen();
        for (field, expect) in [
            (Field::Account, "OWC33ab"),
            (Field::Password, "hunter2ab"),
            (Field::Server, "127.0.0.1ab"),
            (Field::Locale, "enUSab"),
        ] {
            screen.focus = field;
            screen.type_into("ab");
            assert_eq!(screen.field_text(field), expect, "{field:?}");
        }
        screen.focus = Field::Data;
        screen.type_into("X");
        assert_eq!(screen.field_text(Field::Data), "D:/Games/WoW/DataX");
        screen.backspace();
        assert_eq!(screen.field_text(Field::Data), "D:/Games/WoW/Data");
    }

    /// An emptied data box means "not set". `PathBuf::from("")` is the
    /// current directory, which opens no archives while looking exactly like
    /// a directory somebody chose.
    #[test]
    fn emptying_the_data_box_unsets_it() {
        let mut screen = screen();
        screen.settings.data = Some(PathBuf::from("D"));
        screen.focus = Field::Data;
        screen.backspace();
        assert_eq!(screen.settings.data, None);
    }

    #[test]
    fn control_characters_never_reach_a_field() {
        let mut screen = screen();
        screen.focus = Field::Account;
        screen.settings.account.clear();
        screen.type_into("a\r\n\u{1b}b");
        assert_eq!(screen.settings.account, "ab");
    }

    /// Tab cycles the panel that is showing, and *only* that panel: a tab in
    /// the sign-in form that landed on the locale box would be typing a realm
    /// name into a setting.
    #[test]
    fn tab_stays_within_the_open_panel() {
        let mut screen = screen();
        screen.focus = Field::Account;
        for expect in [Field::Password, Field::Server, Field::Account] {
            screen.key(egui::Key::Tab, egui::Modifiers::NONE);
            assert_eq!(screen.focus, expect);
        }
        screen.key(egui::Key::Tab, egui::Modifiers::SHIFT);
        assert_eq!(screen.focus, Field::Server);

        screen.settings_open = true;
        screen.focus = Field::Data;
        screen.key(egui::Key::Tab, egui::Modifiers::NONE);
        assert_eq!(screen.focus, Field::Locale);
        screen.key(egui::Key::Tab, egui::Modifiers::NONE);
        assert_eq!(screen.focus, Field::Data);
    }

    /// Enter in a half-filled form moves on rather than spending a round trip
    /// to be told what the screen already knew.
    #[test]
    fn enter_advances_before_it_submits() {
        let mut screen = screen();
        screen.password.clear();
        screen.focus = Field::Account;
        assert_eq!(screen.submit(), Action::None);
        assert_eq!(screen.focus, Field::Password);
        screen.password = "x".into();
        assert_eq!(screen.submit(), Action::SignIn);
    }

    /// A row that cannot act must not answer, in the list *and* in the button
    /// -- the rule every list frame here follows, because the alternative
    /// ships a request the server declines in silence.
    #[test]
    fn a_blocked_row_is_refused_by_both_paths() {
        let mut screen = screen();
        screen.show_characters(vec![
            CharacterRow {
                name: "Testwolf".into(),
                detail: "12 Human Warrior".into(),
                blocked: false,
            },
            CharacterRow {
                name: "Xx".into(),
                detail: "needs a rename".into(),
                blocked: true,
            },
        ]);
        screen.selected = 1;
        assert_eq!(screen.submit(), Action::None);
        let panel = screen.panel(&Style::default());
        let blocked = panel
            .items
            .iter()
            .find(|i| matches!(i, Item::Row { index: 1, .. }))
            .expect("the blocked row is drawn");
        assert_eq!(blocked.target(), None);
        let enter = panel
            .items
            .iter()
            .find(|i| matches!(i, Item::Button { target: Target::Choose, .. }))
            .expect("the enter button is drawn");
        assert_eq!(enter.target(), None, "a disabled button must not act");

        screen.selected = 0;
        assert_eq!(screen.submit(), Action::Choose(0));
    }

    /// The last character played is picked out, so Enter twice is the whole
    /// sign-in for somebody who plays one character.
    #[test]
    fn the_remembered_character_is_preselected() {
        let mut screen = screen();
        screen.settings.character = Some("watcher".into());
        screen.show_characters(vec![
            CharacterRow { name: "Testwolf".into(), detail: String::new(), blocked: false },
            CharacterRow { name: "Watcher".into(), detail: String::new(), blocked: false },
        ]);
        assert_eq!(screen.selected, 1);
        // And a character that is gone falls back to the first rather than to
        // an index past the end.
        screen.settings.character = Some("Deleted".into());
        screen.show_characters(vec![CharacterRow {
            name: "Testwolf".into(),
            detail: String::new(),
            blocked: false,
        }]);
        assert_eq!(screen.selected, 0);
    }

    /// A list longer than the window says so. The auction window's rule, in
    /// the frame where an account with fifty characters meets a panel with
    /// room for nine.
    #[test]
    fn a_long_list_says_it_is_a_window() {
        let mut screen = screen();
        let many: Vec<CharacterRow> = (0..30)
            .map(|i| CharacterRow {
                name: format!("Alt{i}"),
                detail: String::new(),
                blocked: false,
            })
            .collect();
        screen.show_characters(many);
        let panel = screen.panel(&Style::default());
        let notes: Vec<&String> = panel
            .items
            .iter()
            .filter_map(|i| match i {
                Item::Note { text, .. } => Some(text),
                _ => None,
            })
            .collect();
        assert!(
            notes.iter().any(|n| n.contains("of 30")),
            "the panel must say the list is clipped: {notes:?}"
        );
        let rows = panel
            .items
            .iter()
            .filter(|i| matches!(i, Item::Row { .. }))
            .count();
        assert_eq!(rows, VISIBLE_ROWS);
    }

    /// Selecting past the bottom scrolls the window rather than picking a row
    /// nobody can see.
    #[test]
    fn the_window_follows_the_selection() {
        let mut screen = screen();
        screen.show_characters(
            (0..30)
                .map(|i| CharacterRow {
                    name: format!("Alt{i}"),
                    detail: String::new(),
                    blocked: false,
                })
                .collect(),
        );
        for _ in 0..29 {
            screen.move_selection(1);
        }
        assert_eq!(screen.selected, 29);
        let panel = screen.panel(&Style::default());
        let shown: Vec<usize> = panel
            .items
            .iter()
            .filter_map(|i| match i {
                Item::Row { index, .. } => Some(*index),
                _ => None,
            })
            .collect();
        assert!(shown.contains(&29), "{shown:?}");
        // And it cannot walk off either end.
        for _ in 0..100 {
            screen.move_selection(-1);
        }
        assert_eq!(screen.selected, 0);
    }

    /// Nothing on the panel may overlap anything else on it. Two items
    /// sharing pixels is a click that reaches whichever the search finds
    /// first, which is the invite prompt's bug in a panel with eleven
    /// controls instead of two.
    #[test]
    fn no_two_items_overlap() {
        let style = Style::default();
        let mut screen = screen();
        let mut panels = vec![screen.panel(&style)];
        screen.settings_open = true;
        panels.push(screen.panel(&style));
        screen.settings_open = false;
        screen.working("signing in");
        panels.push(screen.panel(&style));
        screen.show_realms(vec![RealmRow {
            name: "AzerothCore".into(),
            detail: "online".into(),
            offline: false,
        }]);
        panels.push(screen.panel(&style));
        screen.show_characters(vec![CharacterRow {
            name: "Testwolf".into(),
            detail: String::new(),
            blocked: false,
        }]);
        panels.push(screen.panel(&style));

        for panel in &panels {
            for (i, a) in panel.items.iter().enumerate() {
                assert!(
                    panel.size.y >= a.rect().bottom(),
                    "an item is drawn past the panel: {:?} in {:?}",
                    a.rect(),
                    panel.size
                );
                for b in panel.items.iter().skip(i + 1) {
                    let overlap = a.rect().intersect(b.rect());
                    assert!(
                        !overlap.is_positive(),
                        "{:?} overlaps {:?}",
                        a.rect(),
                        b.rect()
                    );
                }
            }
        }
    }

    /// A failure has to leave the waiting stage, or the screen says "signing
    /// in" forever with no button on it.
    #[test]
    fn a_failure_gives_the_panel_back() {
        let mut screen = screen();
        screen.working("signing in");
        assert!(screen.panel(&Style::default()).items.iter().all(|i| !matches!(
            i,
            Item::Button { target: Target::SignIn, .. }
        )));
        screen.failed("account exists but the password was rejected");
        assert_eq!(screen.stage, Stage::Credentials);
        assert!(matches!(screen.status, Some((_, Tone::Bad))));
        assert!(screen.panel(&Style::default()).items.iter().any(|i| matches!(
            i,
            Item::Button { target: Target::SignIn, .. }
        )));
    }

    /// Every theme's accent gets readable text on the button it fills.
    #[test]
    fn the_primary_button_stays_readable() {
        for theme in Theme::ALL {
            let accent = theme.style().login_accent;
            let text = contrasting(accent);
            let [r, g, b, _] = accent.0;
            let bg = 0.299 * r as f32 + 0.587 * g as f32 + 0.114 * b as f32;
            let fg = 0.299 * text.r() as f32 + 0.587 * text.g() as f32 + 0.114 * text.b() as f32;
            assert!(
                (bg - fg).abs() > 90.0,
                "{}: accent luma {bg}, text luma {fg}",
                theme.name()
            );
        }
    }

    /// Runs the real [`SignIn::show`] through a headless egui context and
    /// returns every string that reached the screen, plus what it reported.
    ///
    /// **The layer the tests above cannot see.** They walk `panel()` and call
    /// `submit()` directly, which asserts the arithmetic and the state machine
    /// and would pass with `show` never painting a pixel -- the gap that let
    /// 4.24 ship on "1,004 tests green plus a clean live render" while the
    /// interface was a white screen. So this drives the function the window
    /// drives.
    /// `passes` is **two for drawing and four for clicking**, which is not a
    /// magic number but a fact about egui worth stating: a press is matched
    /// against the widget rectangles from the pass *before* it, so on a fresh
    /// context there are none and the first press lands on nothing. The HUD's
    /// own harness carries the same split for the same reason. Written as two
    /// here first, which made every click test report the panel's initial
    /// state and look exactly like a hit test that was simply wrong.
    fn run(screen: &mut SignIn, events: Vec<egui::Event>, passes: usize) -> (Vec<String>, Action) {
        let ctx = egui::Context::default();
        let input = egui::RawInput {
            screen_rect: Some(Rect::from_min_size(Pos2::ZERO, SCREEN)),
            events,
            ..Default::default()
        };
        let style = Style::default();
        let (mut text, mut action) = (Vec::new(), Action::None);
        for _ in 0..passes {
            let output = ctx.run_ui(input.clone(), |ctx| {
                action = screen.show(ctx, &style);
            });
            text = painted_text(&output.shapes);
            output.drop_without_applying_deltas();
        }
        (text, action)
    }

    const SCREEN: Vec2 = Vec2::new(1280.0, 720.0);

    fn painted_text(shapes: &[egui::epaint::ClippedShape]) -> Vec<String> {
        fn walk(shape: &egui::Shape, out: &mut Vec<String>) {
            match shape {
                egui::Shape::Text(text) => out.push(text.galley.text().to_string()),
                egui::Shape::Vec(shapes) => shapes.iter().for_each(|s| walk(s, out)),
                _ => {}
            }
        }
        let mut out = Vec::new();
        for clipped in shapes {
            walk(&clipped.shape, &mut out);
        }
        out
    }

    #[test]
    fn the_panel_paints_what_it_describes() {
        let mut screen = screen();
        let (text, action) = run(&mut screen, Vec::new(), 2);
        assert_eq!(action, Action::None);
        for wanted in ["Account", "Password", "Realm server", "Sign In", "Quit"] {
            assert!(text.iter().any(|t| t == wanted), "{wanted:?} in {text:?}");
        }
        // What was typed is on screen, and the password is not.
        assert!(text.iter().any(|t| t == "OWC33"), "{text:?}");
        assert!(text.iter().any(|t| t == "127.0.0.1"), "{text:?}");
        assert!(
            !text.iter().any(|t| t.contains("hunter2")),
            "the password was painted in the clear: {text:?}"
        );
        let bullet = '\u{2022}';
        assert!(
            text.iter()
                .any(|t| !t.is_empty() && t.chars().all(|c| c == bullet)),
            "the password box drew no bullets: {text:?}"
        );
    }

    /// A character list has to reach the screen with its detail lines on it,
    /// and with the note saying where new characters come from -- which is the
    /// one sentence on this screen that answers a question the client cannot.
    #[test]
    fn the_character_list_paints_its_rows() {
        let mut screen = screen();
        screen.show_characters(vec![CharacterRow {
            name: "Testwolf".into(),
            detail: "Level 12 Human Warrior".into(),
            blocked: false,
        }]);
        let (text, _) = run(&mut screen, Vec::new(), 2);
        assert!(text.iter().any(|t| t == "Testwolf"), "{text:?}");
        assert!(text.iter().any(|t| t == "Level 12 Human Warrior"), "{text:?}");
        assert!(text.iter().any(|t| t == "Enter World"), "{text:?}");
        assert!(
            text.iter().any(|t| t.contains("original client")),
            "the panel must say where characters are made: {text:?}"
        );
    }

    /// **Clicking a control does the thing it draws.** A frame that draws
    /// correctly, hit-tests correctly and never reports a click is this
    /// project's most repeated interface bug, and this panel has nine controls
    /// on it.
    #[test]
    fn every_control_reports_the_click_it_draws() {
        let style = Style::default();
        let panel = screen().panel(&style);
        let origin = Pos2::new(
            (SCREEN.x / 2.0 - panel.size.x / 2.0).round(),
            (SCREEN.y / 2.0 - panel.size.y / 2.0).round().max(8.0),
        );

        let mut clicked = 0;
        for item in &panel.items {
            let Some(target) = item.target() else { continue };
            clicked += 1;
            let at = origin + item.rect().center().to_vec2();
            // A fresh screen per control: a click that focused a field would
            // otherwise change what the next one is aimed at.
            let mut screen = screen();
            let (_, action) = run(
                &mut screen,
                vec![
                    egui::Event::PointerMoved(at),
                    egui::Event::PointerButton {
                        pos: at,
                        button: egui::PointerButton::Primary,
                        pressed: true,
                        modifiers: egui::Modifiers::NONE,
                    },
                    egui::Event::PointerButton {
                        pos: at,
                        button: egui::PointerButton::Primary,
                        pressed: false,
                        modifiers: egui::Modifiers::NONE,
                    },
                ],
                4,
            );
            match target {
                // Focusing a field and opening the settings panel are this
                // screen's own business, so they report nothing to the caller
                // -- but they must still have happened.
                Target::Focus(field) => assert_eq!(screen.focus, field, "clicking {field:?}"),
                Target::OpenSettings => assert!(screen.settings_open, "clicking the cat"),
                Target::SignIn => assert_eq!(action, Action::SignIn),
                Target::Quit => assert_eq!(action, Action::Quit),
                other => panic!("unexpected control on the sign-in panel: {other:?}"),
            }
        }
        // Without this the loop passes by testing nothing the day the panel
        // stops offering controls, which is the failure mode of every loop
        // shaped like it.
        assert!(clicked >= 5, "only {clicked} controls were clickable");
    }

    /// Typing reaches the focused field through the real event queue, rather
    /// than through `type_into` directly -- which is where a keystroke egui
    /// swallowed would show up.
    #[test]
    fn keystrokes_reach_the_focused_field() {
        let mut screen = screen();
        screen.focus = Field::Account;
        screen.settings.account.clear();
        let (_, action) = run(&mut screen, vec![egui::Event::Text("Testwolf".into())], 2);
        assert_eq!(action, Action::None);
        // The harness delivers the same input twice, so the text arrives
        // twice: this asserts about the harness as much as about `show`, and
        // an assertion of `"Testwolf"` would be the one that was wrong.
        assert_eq!(screen.settings.account, "TestwolfTestwolf");
    }
}
