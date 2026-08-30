//! The viewer's half of the sign-in screen.
//!
//! `ui::login` draws the panel and owns what has been typed into it; this
//! module is everything that panel is not allowed to do -- open sockets, read
//! archives, and ask the operating system for a folder. The split is the same
//! one the rest of the interface follows: `crates/ui` depends on neither
//! `world` nor `render`, so the panel's geometry, its focus ring and its
//! refusals are all testable without a connection or a GPU, and are.
//!
//! **The logon exchange runs on a thread.** Not for speed -- it is three round
//! trips -- but because the common way to get it wrong is a hostname that
//! resolves to nothing, and that spends the full ten-second timeout. Blocking
//! the event loop for ten seconds is a window Windows paints grey and offers
//! to kill, which is the first thing a new user would see after their first
//! typo. Everything after the character list is still synchronous: entering
//! the world reads the archives, which the render thread owns anyway.

use std::sync::mpsc::{self, Receiver};

use mpq::Chain;

use crate::live;

/// The operating system's credential store, used only for the "Save password"
/// option on the sign-in screen.
///
/// **`ui::login` deliberately cannot reach this** -- that crate touches no
/// files but its own settings, and a system keychain is further still. So the
/// panel carries a `save_password` flag and the account+server it keys on, and
/// this module is the half that actually stores the secret.
///
/// Every call is best-effort. A machine with no usable keychain -- a CI
/// runner, a stripped container, a platform this build has no backend for --
/// simply does not remember, which is the same outcome as leaving the box
/// unticked. A failure is logged, never surfaced to the panel: "your password
/// was not saved" is not something to interrupt a login for.
mod secret {
    /// The service name every entry is filed under in the store. The account
    /// and server go in the *user* half of the key -- see `ui::Account::
    /// secret_key`.
    const SERVICE: &str = "open-wow";

    fn entry(key: &str) -> Option<keyring::Entry> {
        match keyring::Entry::new(SERVICE, key) {
            Ok(entry) => Some(entry),
            Err(e) => {
                tracing::warn!("credential store unavailable: {e}");
                None
            }
        }
    }

    /// The saved password for this account+server key, or `None` if there is
    /// none or the store cannot be read.
    pub fn load(key: &str) -> Option<String> {
        match entry(key)?.get_password() {
            Ok(password) => Some(password),
            Err(keyring::Error::NoEntry) => None,
            Err(e) => {
                tracing::warn!("could not read a saved password: {e}");
                None
            }
        }
    }

    pub fn store(key: &str, password: &str) {
        let Some(entry) = entry(key) else { return };
        if let Err(e) = entry.set_password(password) {
            tracing::warn!("could not save the password: {e}");
        }
    }

    pub fn forget(key: &str) {
        let Some(entry) = entry(key) else { return };
        match entry.delete_credential() {
            Ok(()) | Err(keyring::Error::NoEntry) => {}
            Err(e) => tracing::warn!("could not clear a saved password: {e}"),
        }
    }
}

/// The account name and password on their way to a worker thread.
///
/// Named so the password is passed as a field of something with a comment on
/// it rather than as the fourth `String` of six -- and so it is obvious that
/// this value is dropped with the thread rather than stored anywhere.
struct Credentials {
    host: String,
    port: u16,
    user: String,
    password: String,
    locale: String,
}

/// What a worker thread came back with.
///
/// Each carries a `String` on failure rather than the error itself: the error
/// types differ between the two steps, and what the panel needs is the
/// sentence, formatted with `{:#}` so the whole `anyhow` chain survives. A
/// bare `{}` here would print "connection failed" and drop the part that says
/// which host.
enum Done {
    /// The logon exchange finished, with the realm list or a refusal.
    Authenticated(Result<auth::LoggedIn, String>),
    /// The world handshake finished, with the character list or a refusal.
    Realm(Box<Result<(world::Connection, Vec<world::protocol::Character>), String>>),
}

/// A character the world is about to be entered as.
pub struct Entering {
    pub realm: String,
    pub connection: world::Connection,
    pub character: world::protocol::Character,
}

/// What the sign-in screen wants the application to do this frame.
pub enum Outcome {
    /// Still signing in. Draw the panel again next frame.
    Continue,
    Quit,
    /// Repaint the interface in this theme and save the layout.
    Theme(ui::Theme),
    /// The data directory or locale changed; reopen the archives.
    DataChanged,
    /// Enter the world. Boxed because this variant is far larger than the
    /// others and every frame that is *not* entering the world would
    /// otherwise pay for it.
    Enter(Box<Entering>),
}

/// Turns the ids in a character list into words.
///
/// Three tables read once, at the moment a character list is first shown.
/// Read here rather than shared with `crate::maps` -- which already holds
/// `AreaTable` -- because that struct is built for the world map's projection
/// and this is a name lookup that has to survive the archives being *reopened*
/// under it when somebody changes the data directory in the settings panel.
#[derive(Default)]
pub struct Names {
    classes: std::collections::HashMap<u32, String>,
    races: std::collections::HashMap<u32, String>,
    zones: std::collections::HashMap<u32, String>,
}

impl Names {
    pub fn load(chain: &mut Chain) -> Names {
        use dbc::schema::{AreaTable, ChrClasses, ChrRaces};
        let mut names = Names::default();
        // Each table is optional in the same way every other table this client
        // reads is: a missing one costs a word on a row, not a client that
        // will not start. And the chain may hold nothing at all -- before a
        // data directory is chosen, that is exactly what it holds.
        if let Some(table) = chain.read(ChrClasses::PATH).ok().and_then(|b| ChrClasses::parse(&b).ok())
        {
            names.classes = table.iter().map(|r| (r.id(), r.name().to_string())).collect();
        }
        if let Some(table) = chain.read(ChrRaces::PATH).ok().and_then(|b| ChrRaces::parse(&b).ok()) {
            names.races = table.iter().map(|r| (r.id(), r.name().to_string())).collect();
        }
        if let Some(table) = chain.read(AreaTable::PATH).ok().and_then(|b| AreaTable::parse(&b).ok())
        {
            names.zones = table.iter().map(|r| (r.id(), r.name().to_string())).collect();
        }
        tracing::info!(
            "sign-in name tables: {} classes, {} races, {} areas",
            names.classes.len(),
            names.races.len(),
            names.zones.len()
        );
        names
    }

    /// The line under a character's name.
    ///
    /// **Every part is omitted rather than guessed when its table is
    /// missing.** A client pointed at an install whose `ChrRaces.dbc` will not
    /// read should say `Level 12 Warrior`, not `Level 12 Race 4 Warrior` --
    /// the row this project keeps relearning is that a fabricated value is
    /// believed and a blank is not.
    fn describe(&self, character: &world::protocol::Character) -> String {
        let mut parts = vec![format!("Level {}", character.level)];
        if let Some(race) = self.races.get(&(character.race as u32)) {
            parts.push(race.clone());
        }
        if let Some(class) = self.classes.get(&(character.class as u32)) {
            parts.push(class.clone());
        }
        let mut line = parts.join(" ");
        if let Some(zone) = self.zones.get(&character.zone) {
            line.push_str(" \u{2014} ");
            line.push_str(zone);
        }
        if character.is_ghost() {
            line.push_str(" (dead)");
        }
        line
    }
}

/// The sign-in screen and everything behind it.
pub struct SignIn {
    pub screen: ui::SignIn,
    names: Names,
    /// The worker thread's channel, present exactly while one is running.
    /// **The presence of the channel is the "busy" flag**, so there is no
    /// second boolean that could disagree with it.
    pending: Option<Receiver<Done>>,
    /// The logon session: the realm list and the key the world server expects
    /// its header cipher to be keyed with. Kept because choosing a realm needs
    /// it, and a second logon exchange to get it back would be a second
    /// password prompt.
    session: Option<auth::LoggedIn>,
    /// Which realm was chosen, so the character list knows what to call itself
    /// and `LiveWorld::realm` gets the right name on it -- the name that ends
    /// up in the quest cache's filename.
    realm: Option<auth::Realm>,
    /// The world connection, handshaken and not yet in the world.
    ///
    /// **Held here rather than reopened when a character is picked.** The
    /// header cipher's RC4 state cannot be shared or rewound, so this exact
    /// socket is the only one that can enter the world with the session that
    /// read the character list off it.
    connection: Option<world::Connection>,
    /// The characters behind the rows on screen, in the same order, so an
    /// index means the same thing to the panel and to the wire. The panel gets
    /// names and a description; entering the world needs the guid, the race,
    /// the appearance and the equipment, none of which the panel has any
    /// business holding.
    characters: Option<Vec<world::protocol::Character>>,
}

impl SignIn {
    pub fn new() -> Self {
        let mut this = Self {
            screen: ui::SignIn::new(),
            names: Names::default(),
            pending: None,
            session: None,
            realm: None,
            connection: None,
            characters: None,
        };
        this.reload_saved_password();
        this
    }

    /// Fills the password field from the credential store, if the account the
    /// form is showing has "Save password" set and something is stored for it.
    /// A no-op otherwise -- including when nothing is stored, which leaves the
    /// field empty rather than clearing what a caller may have just put there.
    pub fn reload_saved_password(&mut self) {
        let account = self.screen.settings.account();
        if !account.save_password
            || account.name.trim().is_empty()
            || account.server.trim().is_empty()
        {
            return;
        }
        if let Some(password) = secret::load(&account.secret_key()) {
            self.screen.password = password;
        }
    }

    /// Reads the tables the character list needs. Called after the archives
    /// are opened, and again whenever they are reopened.
    pub fn set_names(&mut self, names: Names) {
        self.names = names;
    }

    /// Draws the panel, drives whatever is in flight, and says what the
    /// application should do.
    pub fn update(&mut self, ctx: &egui::Context, style: &ui::Style) -> Outcome {
        // The worker first, so a reply that arrived between frames is on the
        // panel this frame rather than the next one.
        self.poll();
        match self.screen.show(ctx, style) {
            ui::login::Action::None => Outcome::Continue,
            ui::login::Action::Quit => Outcome::Quit,
            ui::login::Action::Theme(theme) => Outcome::Theme(theme),
            ui::login::Action::DataChanged => Outcome::DataChanged,
            ui::login::Action::BrowseData => {
                self.browse();
                Outcome::Continue
            }
            ui::login::Action::Back => {
                self.back();
                Outcome::Continue
            }
            ui::login::Action::SignIn => {
                self.authenticate();
                Outcome::Continue
            }
            ui::login::Action::Choose(index) => self.choose(index),
            ui::login::Action::AccountChanged => {
                // The panel has already cleared the password. Refill it from
                // the store if this account kept one.
                self.reload_saved_password();
                Outcome::Continue
            }
            ui::login::Action::ForgetPassword => {
                secret::forget(&self.screen.settings.account().secret_key());
                Outcome::Continue
            }
        }
    }

    /// Drops whatever has been reached and returns to the credentials panel.
    ///
    /// **The connection goes with it.** A world connection that has done its
    /// handshake and not entered the world is not reusable for a different
    /// account, and keeping one around to be picked up later is how a session
    /// ends up talking to a realm nobody selected.
    fn back(&mut self) {
        self.pending = None;
        self.session = None;
        self.realm = None;
        self.connection = None;
        self.characters = None;
        self.screen.stage = ui::SignInStage::Credentials;
        self.screen.status = None;
    }

    fn authenticate(&mut self) {
        let (host, port) = self.screen.settings.address();
        let credentials = Credentials {
            host,
            port,
            user: self.screen.settings.account().name.trim().to_string(),
            password: std::mem::take(&mut self.screen.password),
            locale: self.screen.settings.locale.trim().to_string(),
        };
        // Taken rather than copied: the panel is about to stop showing the
        // password field, and a buffer that is not needed again is one less
        // copy of a password sitting in memory. It is put back if the sign-in
        // fails, because retyping it after a typo in the *account* name would
        // be its own small insult.
        let echo = credentials.password.clone();
        self.screen.working(format!(
            "signing in to {}:{}\u{2026}",
            credentials.host, credentials.port
        ));
        self.screen.password = echo;
        self.screen.save();

        let (tx, rx) = mpsc::channel();
        std::thread::spawn(move || {
            let result = live::authenticate(
                &credentials.host,
                credentials.port,
                &credentials.user,
                &credentials.password,
                &credentials.locale,
            )
            .map_err(|e| format!("{e:#}"));
            // The receiver is gone if the user pressed Cancel, and that is
            // ordinary rather than an error: nothing here has to be delivered.
            let _ = tx.send(Done::Authenticated(result));
        });
        self.pending = Some(rx);
    }

    /// A row of whichever list is showing was chosen.
    fn choose(&mut self, index: usize) -> Outcome {
        match self.screen.stage.clone() {
            ui::SignInStage::Realms(_) => {
                let Some(session) = &self.session else {
                    self.screen.failed("the logon session was lost; sign in again");
                    return Outcome::Continue;
                };
                let Some(realm) = session.realms.get(index).cloned() else {
                    self.screen.failed("that realm is no longer in the list");
                    return Outcome::Continue;
                };
                self.open_realm(realm);
                Outcome::Continue
            }
            ui::SignInStage::Characters(_) => self.enter(index),
            _ => Outcome::Continue,
        }
    }

    fn open_realm(&mut self, realm: auth::Realm) {
        let Some(session) = &self.session else { return };
        let key = session.session_key;
        let user = self.screen.settings.account().name.trim().to_string();
        self.screen
            .working(format!("entering {}\u{2026}", realm.name));
        self.screen.settings.account_mut().realm = Some(realm.name.clone());
        self.screen.save();
        self.realm = Some(realm.clone());

        let (tx, rx) = mpsc::channel();
        std::thread::spawn(move || {
            let result =
                live::open_realm(&realm, &user, &key).map_err(|e| format!("{e:#}"));
            let _ = tx.send(Done::Realm(Box::new(result)));
        });
        self.pending = Some(rx);
    }

    /// Hands the chosen character, and the connection it will be played on,
    /// to the application.
    fn enter(&mut self, index: usize) -> Outcome {
        let Some(realm) = self.realm.clone() else {
            self.screen.failed("the realm connection was lost; sign in again");
            return Outcome::Continue;
        };
        // The list on the panel and the characters behind it are the same
        // list, in the same order, so an index means the same thing to both.
        let Some(characters) = self.characters.take() else {
            self.screen.failed("the character list was lost; sign in again");
            return Outcome::Continue;
        };
        let Some(character) = characters.get(index).cloned() else {
            self.characters = Some(characters);
            self.screen.failed("that character is no longer in the list");
            return Outcome::Continue;
        };
        let Some(connection) = self.connection.take() else {
            self.screen.failed("the realm connection was lost; sign in again");
            return Outcome::Continue;
        };
        self.screen.settings.account_mut().character = Some(character.name.clone());
        self.screen.save();
        self.screen
            .working(format!("entering the world as {}\u{2026}", character.name));
        Outcome::Enter(Box::new(Entering {
            realm: realm.name,
            connection,
            character,
        }))
    }

    /// Collects a worker thread's reply, if it has one yet.
    fn poll(&mut self) {
        let Some(rx) = &self.pending else { return };
        let done = match rx.try_recv() {
            Ok(done) => done,
            Err(mpsc::TryRecvError::Empty) => return,
            // The thread panicked. Nothing else would ever fill this channel,
            // so the panel has to be given back rather than left waiting on a
            // reply that cannot arrive -- the failure the sign-in screen's own
            // `failed` test is about.
            Err(mpsc::TryRecvError::Disconnected) => {
                self.pending = None;
                self.screen
                    .failed("the sign-in thread stopped; see the log for a panic");
                return;
            }
        };
        self.pending = None;
        match done {
            Done::Authenticated(Ok(session)) => {
                // The logon server has accepted the credentials, so this is
                // the moment the account is worth remembering and -- if the
                // box is ticked -- the password is worth writing. Ticking it
                // over a typo does not reach here.
                self.screen.remember_active();
                let key = self.screen.settings.account().secret_key();
                if self.screen.settings.account().save_password {
                    if !self.screen.password.is_empty() {
                        secret::store(&key, &self.screen.password);
                    }
                } else {
                    secret::forget(&key);
                }

                let realms = session.realms.clone();
                self.session = Some(session);
                match realms.len() {
                    0 => self
                        .screen
                        .failed("this logon server offered no realms at all"),
                    // One realm is not a choice, so it is not offered as one.
                    1 => self.open_realm(realms[0].clone()),
                    _ => self.screen.show_realms(
                        realms
                            .iter()
                            .map(|realm| ui::RealmRow {
                                name: realm.name.clone(),
                                detail: describe_realm(realm),
                                offline: realm.is_offline(),
                            })
                            .collect(),
                    ),
                }
            }
            Done::Authenticated(Err(e)) => self.screen.failed(e),
            Done::Realm(result) => match *result {
                Ok((connection, characters)) => {
                    self.screen.show_characters(
                        characters
                            .iter()
                            .map(|c| ui::CharacterRow {
                                name: c.name.clone(),
                                detail: self.names.describe(c),
                                // A character the server will refuse to let
                                // in until it is renamed. Refused here rather
                                // than entered and thrown out with a code.
                                blocked: c.needs_rename(),
                            })
                            .collect(),
                    );
                    if characters.is_empty() {
                        self.screen
                            .note("nothing to play here yet");
                    }
                    self.connection = Some(connection);
                    self.characters = Some(characters);
                }
                Err(e) => self.screen.failed(e),
            },
        }
    }

    /// Asks the operating system for a folder.
    fn browse(&mut self) {
        let start = self
            .screen
            .settings
            .data
            .clone()
            .unwrap_or_else(|| std::path::PathBuf::from("."));
        let picked = rfd::FileDialog::new()
            .set_title("Choose the Data folder of a WoW 3.3.5a installation")
            .set_directory(start)
            .pick_folder();
        // A cancelled dialogue is an answer, and the answer is "leave it
        // alone" -- not "clear the setting", which is what writing the
        // `Option` straight through would do.
        if let Some(path) = picked {
            self.screen.settings.data = Some(path);
            self.screen.save();
        }
    }
}

/// What a realm row says under its name.
fn describe_realm(realm: &auth::Realm) -> String {
    if realm.is_offline() {
        return "offline".into();
    }
    // The population is a float the server sets to a load figure; what it
    // means numerically is not documented anywhere this project trusts, so it
    // is described in the terms the original client used rather than printed.
    let load = match realm.population {
        p if p < 0.5 => "low",
        p if p < 1.5 => "medium",
        _ => "high",
    };
    format!("{} characters \u{00b7} {load} population", realm.characters)
}

impl Default for SignIn {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A character list row, written out in full.
    ///
    /// **Not `mem::zeroed`**, which is what this was first written as and
    /// which took the whole test binary down with a stack-buffer-overrun: a
    /// zeroed `String` is a null pointer wearing a `String`'s shape, and the
    /// `Vec` behind it must be non-null. `Character` is a wire struct with no
    /// `Default`, deliberately, so the cost of one is writing it out.
    fn character(name: &str, race: u8, class: u8, zone: u32) -> world::protocol::Character {
        world::protocol::Character {
            guid: 1,
            name: name.to_string(),
            race,
            class,
            gender: 0,
            skin: 0,
            face: 0,
            hair_style: 0,
            hair_color: 0,
            facial_hair: 0,
            level: 12,
            zone,
            map: 0,
            position: [0.0; 3],
            guild_id: 0,
            flags: 0,
            customize_flags: 0,
            first_login: false,
            pet_display_id: 0,
            pet_level: 0,
            pet_family: 0,
            equipment: [world::protocol::Equipment::default();
                world::protocol::EQUIPMENT_SLOTS],
        }
    }

    fn names() -> Names {
        Names {
            classes: [(1, "Warrior".to_string()), (11, "Druid".to_string())]
                .into_iter()
                .collect(),
            races: [(1, "Human".to_string()), (4, "Night Elf".to_string())]
                .into_iter()
                .collect(),
            zones: [(9, "Northshire Valley".to_string())].into_iter().collect(),
        }
    }

    #[test]
    fn a_character_reads_as_a_sentence() {
        assert_eq!(
            names().describe(&character("Testwolf", 1, 1, 9)),
            "Level 12 Human Warrior \u{2014} Northshire Valley"
        );
    }

    /// **A name that will not resolve is left out, never invented.** A client
    /// pointed at an install whose `ChrRaces.dbc` does not read should say
    /// less, not say `Race 7` -- a fabricated value is believed and a blank is
    /// not, which is the rule `describe_cast_failure` and the description
    /// substituter both already follow.
    #[test]
    fn an_unresolvable_id_is_omitted_rather_than_printed() {
        let names = names();
        let line = names.describe(&character("Gnomey", 7, 8, 4242));
        assert_eq!(line, "Level 12");
        assert!(!line.contains('7') && !line.contains('8') && !line.contains("4242"), "{line}");

        // And with no tables at all it still says the one thing it knows.
        assert_eq!(
            Names::default().describe(&character("Anyone", 1, 1, 9)),
            "Level 12"
        );
    }

    /// A realm's row has to say something in every state, including the one
    /// where picking it is refused.
    #[test]
    fn every_realm_row_says_something() {
        let mut realm = auth::Realm {
            name: "AzerothCore".into(),
            address: "127.0.0.1:8085".into(),
            kind: 0,
            locked: false,
            flags: 0,
            population: 0.0,
            characters: 4,
            timezone: 1,
            id: 1,
        };
        assert!(describe_realm(&realm).contains("4 characters"));
        assert!(describe_realm(&realm).contains("low"));
        realm.population = 2.0;
        assert!(describe_realm(&realm).contains("high"));
        realm.flags = 0x02;
        assert_eq!(describe_realm(&realm), "offline");
    }
}
