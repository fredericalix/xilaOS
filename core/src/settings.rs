//! The settings that are kept from one start to the next.
//!
//! The keyboard layout, the theme, the mood, the brightness of the screen
//! and how pictures get to it: what the user chose once and expects to
//! find again. They are rows of a stored table, `kept_settings`, with a
//! name and a value each, as text. The view `settings` shows what is in
//! use now; this table is what the next start will begin with.
//!
//! Nothing has to ask for a setting to be kept. `keep` compares what is in
//! use with what was last written and writes what differs; the interface
//! calls it again and again. So a setting is kept whoever changed it: a
//! command, a key, a program.
//!
//! Only a database on a disk gets the table. Tables in memory are lost
//! with the settings they would hold.

use core::fmt::Write;

use xila_db::{Database, Field, Kind};

use crate::keys::Layout;
use crate::shell::{MoodMode, System, Text};
use crate::theme::THEMES;

/// The table the settings are kept in.
pub const TABLE: &[u8] = b"kept_settings";

/// What is kept, as it is in use.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Kept {
    theme: usize,
    layout: Layout,
    mood: MoodMode,
    /// The brightness of the screen in hundredths, once the user set it.
    light: Option<u32>,
    /// The user asked for the screen to be written directly.
    direct: bool,
    /// The companion was turned off.
    quiet: bool,
}

impl Kept {
    /// What a system starts with when nothing is kept.
    const FIRST: Kept = Kept { theme: 0, layout: Layout::Us, mood: MoodMode::Auto, light: None, direct: false, quiet: false };

    fn of(sys: &System) -> Kept {
        Kept { theme: sys.theme, layout: sys.layout, mood: sys.mood, light: sys.display.light, direct: sys.display.direct, quiet: sys.companion.off }
    }

    /// The settings as rows: a name and a value. A setting that was never
    /// chosen has no row.
    fn rows(&self) -> [(&'static [u8], Option<Text>); 6] {
        let text = |bytes: &[u8]| Some(Text::from_bytes(bytes));
        let light = self.light.map(|hundredths| {
            let mut value = Text::new();
            let _ = write!(value, "{hundredths}");
            value
        });
        [
            (b"keymap", text(self.layout.name())),
            (b"theme", text(THEMES[self.theme].name)),
            (b"mood", text(self.mood.name())),
            (b"brightness", light),
            (b"pictures", text(if self.direct { b"direct" } else { b"two" })),
            (b"companion", text(if self.quiet { b"off" } else { b"on" })),
        ]
    }

    /// Takes a row that was kept. A name or a value this system does not
    /// know is left aside: the table may come from a newer one.
    fn take(&mut self, name: &[u8], value: &[u8]) -> bool {
        match (name, value) {
            (b"keymap", b"us") => self.layout = Layout::Us,
            (b"keymap", b"fr") => self.layout = Layout::Fr,
            (b"mood", b"calm") => self.mood = MoodMode::Calm,
            (b"mood", b"auto") => self.mood = MoodMode::Auto,
            (b"mood", b"intense") => self.mood = MoodMode::Intense,
            (b"pictures", b"direct") => self.direct = true,
            (b"pictures", b"two") => self.direct = false,
            (b"companion", b"off") => self.quiet = true,
            (b"companion", b"on") => self.quiet = false,
            (b"theme", _) => match THEMES.iter().position(|theme| theme.name == value) {
                Some(theme) => self.theme = theme,
                None => return false,
            },
            (b"brightness", _) => match crate::text::parse_number(value).filter(|hundredths| (1..=100).contains(hundredths)) {
                Some(hundredths) => self.light = Some(hundredths as u32),
                None => return false,
            },
            _ => return false,
        }
        true
    }
}

/// The table of the settings, if it is there and is what it should be:
/// somebody may have made a table of that name for something else.
fn table(db: &Database) -> Option<usize> {
    let table = db.find(TABLE)?;
    let columns = &db.tables()[table].columns;
    let wanted: [(&[u8], Kind); 3] = [(b"id", Kind::Int), (b"name", Kind::Text), (b"value", Kind::Text)];
    (columns.len() == wanted.len() && columns.iter().zip(&wanted).all(|(column, (name, kind))| column.name == *name && column.kind == *kind)).then_some(table)
}

/// Puts the settings that were kept in use, when the system starts or its
/// database changes. Returns how many there were.
pub fn load(sys: &mut System) -> usize {
    let mut kept = Kept::of(sys);
    let mut count = 0;
    if let Some(table) = table(&sys.db) {
        let _ = sys.db.scan(table, &mut |fields| {
            if let (Field::Text(name), Field::Text(value)) = (fields[1], fields[2]) {
                count += kept.take(name, value) as usize;
            }
        });
    }
    (sys.theme, sys.layout, sys.mood) = (kept.theme, kept.layout, kept.mood);
    (sys.display.light, sys.display.direct) = (kept.light, kept.direct);
    sys.companion.off = kept.quiet;
    // what is in use is what is kept: nothing to write
    sys.kept = Some(kept);
    count
}

/// Writes the settings that are not as they were last written. Does
/// nothing when nothing changed, which is nearly always.
pub fn keep(sys: &mut System) {
    let now = Kept::of(sys);
    if sys.kept == Some(now) {
        return;
    }
    // A database that was just given a disk holds nothing yet: every
    // setting that is not as a system starts is to be written.
    let before = sys.kept.replace(now).unwrap_or(Kept::FIRST);
    if before == now || !sys.db.storage().keeps {
        return;
    }
    if write(&mut sys.db, &before, &now).is_err() {
        // a disk that fails says so when a table is changed; settings are not worth more words
        sys.db.rollback();
    } else {
        sys.stored_at = sys.ticks.max(1);
    }
}

fn write(db: &mut Database, before: &Kept, now: &Kept) -> Result<(), xila_db::Error> {
    let table = match db.find(TABLE) {
        // a table of that name made for something else is left alone
        Some(_) => table(db).ok_or(xila_db::Error::Exists)?,
        None => db.create(TABLE, &[(b"name", Kind::Text), (b"value", Kind::Text)])?,
    };
    for ((name, was), (_, is)) in before.rows().iter().zip(now.rows()) {
        if was.as_ref().map(|value| value.bytes()) == is.as_ref().map(|value| value.bytes()) {
            continue;
        }
        db.delete(table, &mut |fields| matches!(fields[1], Field::Text(kept) if kept == *name))?;
        if let Some(value) = is {
            db.insert(table, &[Field::Text(name), Field::Text(value.bytes())])?;
        }
    }
    db.commit(&mut |_, _, _| {})
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::shell::tests::{run_line, Used};

    /// A system whose tables are on a disk.
    fn on_disk() -> System {
        let mut sys = System::new();
        let Ok(db) = Database::format(xila_db::Volume::single(Box::new(Used(xila_db::Memory::new(2560))))) else {
            panic!("an empty disk was refused");
        };
        sys.db = db;
        assert_eq!(load(&mut sys), 0);
        sys
    }

    fn rows(sys: &mut System) -> Vec<String> {
        run_line(sys, "select name, value from kept_settings order by name").1.lines[1..].iter().map(|line| line.split_whitespace().collect::<Vec<_>>().join("=")).collect()
    }

    #[test]
    fn what_the_user_chose_is_kept_and_found_again() {
        let mut sys = on_disk();
        // nothing was chosen: nothing is kept, not even a table
        keep(&mut sys);
        assert!(sys.db.find(TABLE).is_none());
        run_line(&mut sys, "keymap fr");
        keep(&mut sys);
        assert_eq!(rows(&mut sys), ["keymap=fr"]);
        // whoever changes a setting: a command, or the key that changes the mood
        run_line(&mut sys, "theme night");
        sys.mood = sys.mood.next();
        sys.display.light = Some(60);
        sys.display.direct = true;
        keep(&mut sys);
        assert_eq!(rows(&mut sys), ["brightness=60", "keymap=fr", "mood=intense", "pictures=direct", "theme=night"]);
        let transaction = sys.db.storage().transaction;
        keep(&mut sys);
        keep(&mut sys);
        assert_eq!(sys.db.storage().transaction, transaction, "nothing changed, nothing is written");
        // back to what a system starts with is a choice too
        run_line(&mut sys, "keymap us");
        sys.display.direct = false;
        keep(&mut sys);
        assert_eq!(rows(&mut sys), ["brightness=60", "keymap=us", "mood=intense", "pictures=two", "theme=night"]);

        // the next start: a system as new, with the same database
        let db = core::mem::replace(&mut sys.db, Database::new());
        let mut next = System::new();
        next.db = db;
        assert_eq!(load(&mut next), 5);
        assert_eq!((next.theme, next.layout, next.mood, next.display.light, next.display.direct), (1, Layout::Us, MoodMode::Intense, Some(60), false));
        let transaction = next.db.storage().transaction;
        keep(&mut next);
        assert_eq!(next.db.storage().transaction, transaction, "what was just read is not written again");
    }

    #[test]
    fn tables_in_memory_keep_no_settings() {
        let mut sys = System::new();
        assert_eq!(load(&mut sys), 0);
        run_line(&mut sys, "keymap fr");
        keep(&mut sys);
        assert!(sys.db.find(TABLE).is_none());
        // tables that get a disk and settings as a system starts: still no table
        let mut plain = on_disk();
        plain.kept = None;
        keep(&mut plain);
        assert!(plain.db.find(TABLE).is_none());
        // the day the tables get a disk, what is in use is written to it
        let Ok(db) = Database::format(xila_db::Volume::single(Box::new(Used(xila_db::Memory::new(2560))))) else {
            panic!("an empty disk was refused");
        };
        sys.db = db;
        sys.kept = None;
        keep(&mut sys);
        assert_eq!(rows(&mut sys), ["keymap=fr"]);
    }

    #[test]
    fn what_this_system_does_not_know_is_left_aside() {
        let mut sys = on_disk();
        run_line(&mut sys, "create table kept_settings (name text, value text)");
        for (name, value) in [("keymap", "dvorak"), ("theme", "night"), ("brightness", "0"), ("brightness", "250"), ("sound", "loud"), ("mood", "calm")] {
            assert!(run_line(&mut sys, &format!("insert into kept_settings values ('{name}', '{value}')")).0);
        }
        assert_eq!(load(&mut sys), 2);
        assert_eq!((sys.theme, sys.layout, sys.mood, sys.display.light), (1, Layout::Us, MoodMode::Calm, None));
        // and stays in the table when a setting is written
        run_line(&mut sys, "keymap fr");
        keep(&mut sys);
        let kept = rows(&mut sys);
        assert!(kept.contains(&"sound=loud".to_string()) && kept.contains(&"keymap=fr".to_string()) && !kept.contains(&"keymap=dvorak".to_string()), "{kept:?}");

        // a table of that name made for something else is not touched
        let mut sys = on_disk();
        run_line(&mut sys, "create table kept_settings (what int)");
        run_line(&mut sys, "insert into kept_settings values (7)");
        assert_eq!(load(&mut sys), 0);
        run_line(&mut sys, "theme night");
        keep(&mut sys);
        assert_eq!(run_line(&mut sys, "select what from kept_settings").1.lines[1..], ["   7"]);
        assert!(run_line(&mut sys, "create table after (n int)").0, "the database is not left in a change");
    }
}
