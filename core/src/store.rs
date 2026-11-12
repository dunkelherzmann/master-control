use std::fmt;
use std::path::Path;

use rusqlite::{params_from_iter, types::Value as Sql, Connection};

use crate::{Class, Event};

#[derive(Debug)]
pub enum StoreError {
    Sqlite(rusqlite::Error),
    Json(serde_json::Error),
}

impl fmt::Display for StoreError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            StoreError::Sqlite(error) => write!(f, "event store: {error}"),
            StoreError::Json(error) => write!(f, "event store: {error}"),
        }
    }
}

impl std::error::Error for StoreError {}

impl From<rusqlite::Error> for StoreError {
    fn from(error: rusqlite::Error) -> Self {
        StoreError::Sqlite(error)
    }
}

impl From<serde_json::Error> for StoreError {
    fn from(error: serde_json::Error) -> Self {
        StoreError::Json(error)
    }
}

/// Which events to return. Empty fields match everything.
#[derive(Debug, Clone, Default)]
pub struct Filter {
    pub classes: Vec<Class>,
    pub pid: Option<u32>,
    pub op: Option<String>,
    /// Part of the subject, matched without regard to case.
    pub subject_contains: Option<String>,
    /// Part of the subject, operation, process name, result or arguments.
    pub text: Option<String>,
    pub from_ns: Option<u64>,
    pub until_ns: Option<u64>,
    pub outside: Option<bool>,
    pub limit: Option<u32>,
    pub offset: u32,
}

/// One session's events, in a SQLite file.
pub struct Store {
    db: Connection,
}

impl Store {
    pub fn open(path: &Path) -> Result<Store, StoreError> {
        Store::set_up(Connection::open(path)?)
    }

    pub fn in_memory() -> Result<Store, StoreError> {
        Store::set_up(Connection::open_in_memory()?)
    }

    fn set_up(db: Connection) -> Result<Store, StoreError> {
        db.execute_batch(
            "PRAGMA busy_timeout = 5000;
             PRAGMA journal_mode = WAL;
             PRAGMA synchronous = NORMAL;
             CREATE TABLE IF NOT EXISTS events (
                 id INTEGER PRIMARY KEY,
                 time_ns INTEGER NOT NULL,
                 pid INTEGER NOT NULL,
                 tid INTEGER NOT NULL,
                 process TEXT NOT NULL,
                 class TEXT NOT NULL,
                 op TEXT NOT NULL,
                 subject TEXT NOT NULL,
                 args TEXT NOT NULL,
                 result TEXT NOT NULL,
                 duration_ns INTEGER NOT NULL,
                 stack TEXT NOT NULL,
                 outside INTEGER NOT NULL
             );
             CREATE INDEX IF NOT EXISTS events_time ON events (time_ns);
             CREATE INDEX IF NOT EXISTS events_class ON events (class, time_ns);
             CREATE INDEX IF NOT EXISTS events_pid ON events (pid, time_ns);",
        )?;
        Ok(Store { db })
    }

    /// Adds events in one transaction.
    pub fn insert(&mut self, events: &[Event]) -> Result<(), StoreError> {
        let transaction = self.db.transaction()?;
        {
            let mut statement = transaction.prepare_cached(
                "INSERT INTO events (time_ns, pid, tid, process, class, op, subject, args, result, duration_ns,
                                     stack, outside)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)",
            )?;
            for event in events {
                statement.execute(rusqlite::params![
                    event.time_ns as i64,
                    event.pid,
                    event.tid,
                    event.process,
                    event.class.as_str(),
                    event.op,
                    event.subject,
                    serde_json::to_string(&event.args)?,
                    event.result,
                    event.duration_ns as i64,
                    serde_json::to_string(&event.stack)?,
                    event.outside,
                ])?;
            }
        }
        transaction.commit()?;
        Ok(())
    }

    fn conditions(filter: &Filter) -> (String, Vec<Sql>) {
        let mut parts: Vec<String> = Vec::new();
        let mut values: Vec<Sql> = Vec::new();
        if !filter.classes.is_empty() {
            let marks = vec!["?"; filter.classes.len()].join(", ");
            parts.push(format!("class IN ({marks})"));
            values.extend(filter.classes.iter().map(|class| Sql::Text(class.as_str().to_string())));
        }
        if let Some(pid) = filter.pid {
            parts.push("pid = ?".into());
            values.push(Sql::Integer(pid.into()));
        }
        if let Some(op) = &filter.op {
            parts.push("op = ?".into());
            values.push(Sql::Text(op.clone()));
        }
        if let Some(part) = &filter.subject_contains {
            parts.push("instr(lower(subject), lower(?)) > 0".into());
            values.push(Sql::Text(part.clone()));
        }
        if let Some(text) = &filter.text {
            parts.push(
                "instr(lower(subject || char(10) || op || char(10) || process || char(10) || result || char(10) || args),
                       lower(?)) > 0"
                    .into(),
            );
            values.push(Sql::Text(text.clone()));
        }
        if let Some(from) = filter.from_ns {
            parts.push("time_ns >= ?".into());
            values.push(Sql::Integer(from as i64));
        }
        if let Some(until) = filter.until_ns {
            parts.push("time_ns < ?".into());
            values.push(Sql::Integer(until as i64));
        }
        if let Some(outside) = filter.outside {
            parts.push("outside = ?".into());
            values.push(Sql::Integer(outside.into()));
        }
        let clause = if parts.is_empty() { String::new() } else { format!(" WHERE {}", parts.join(" AND ")) };
        (clause, values)
    }

    /// Matching events, oldest first.
    pub fn query(&self, filter: &Filter) -> Result<Vec<Event>, StoreError> {
        let (clause, mut values) = Store::conditions(filter);
        values.push(Sql::Integer(filter.limit.map_or(-1, i64::from)));
        values.push(Sql::Integer(filter.offset.into()));
        let mut statement = self.db.prepare(&format!(
            "SELECT time_ns, pid, tid, process, class, op, subject, args, result, duration_ns, stack, outside
             FROM events{clause} ORDER BY time_ns, id LIMIT ? OFFSET ?"
        ))?;
        let rows = statement.query_map(params_from_iter(values), |row| {
            Ok((
                row.get::<_, i64>(0)?,
                row.get::<_, u32>(1)?,
                row.get::<_, u32>(2)?,
                row.get::<_, String>(3)?,
                row.get::<_, String>(4)?,
                row.get::<_, String>(5)?,
                row.get::<_, String>(6)?,
                row.get::<_, String>(7)?,
                row.get::<_, String>(8)?,
                row.get::<_, i64>(9)?,
                row.get::<_, String>(10)?,
                row.get::<_, bool>(11)?,
            ))
        })?;
        let mut events = Vec::new();
        for row in rows {
            let (time_ns, pid, tid, process, class, op, subject, args, result, duration_ns, stack, outside) = row?;
            events.push(Event {
                time_ns: time_ns as u64,
                pid,
                tid,
                process,
                // Only this code writes the column, so an unknown name means a newer writer: keep the event.
                class: Class::parse(&class).unwrap_or(Class::Log),
                op,
                subject,
                args: serde_json::from_str(&args)?,
                result,
                duration_ns: duration_ns as u64,
                stack: serde_json::from_str(&stack)?,
                outside,
            });
        }
        Ok(events)
    }

    /// How many events each class has.
    pub fn count_by_class(&self) -> Result<Vec<(String, u64)>, StoreError> {
        let mut statement = self.db.prepare("SELECT class, count(*) FROM events GROUP BY class ORDER BY class")?;
        let rows = statement.query_map([], |row| Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)? as u64)))?;
        Ok(rows.collect::<Result<_, _>>()?)
    }

    /// The things a class's events were about, for the given operations, with how many events each had: the most
    /// touched first.
    pub fn subjects(&self, class: Class, ops: &[&str], limit: u32) -> Result<Vec<(String, u64)>, StoreError> {
        let marks = vec!["?"; ops.len()].join(", ");
        let mut values = vec![Sql::Text(class.as_str().to_string())];
        values.extend(ops.iter().map(|op| Sql::Text(op.to_string())));
        values.push(Sql::Integer(limit.into()));
        let mut statement = self.db.prepare(&format!(
            "SELECT subject, count(*) FROM events WHERE class = ? AND op IN ({marks}) AND subject != ''
             GROUP BY subject ORDER BY count(*) DESC, subject LIMIT ?"
        ))?;
        let rows = statement.query_map(params_from_iter(values), |row| Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)? as u64)))?;
        Ok(rows.collect::<Result<_, _>>()?)
    }

    /// How many events match, ignoring the filter's limit and offset.
    pub fn count(&self, filter: &Filter) -> Result<u64, StoreError> {
        let (clause, values) = Store::conditions(filter);
        let count: i64 =
            self.db.query_row(&format!("SELECT count(*) FROM events{clause}"), params_from_iter(values), |row| row.get(0))?;
        Ok(count as u64)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn sample() -> Vec<Event> {
        let mut open = Event::new(1_000, 42, Class::File, "open", "/home/test/Notes.txt");
        open.process = "editor".into();
        open.args = json!({"access": "read"});
        open.result = "ok".into();
        open.stack = vec!["open".into(), "main".into()];
        let mut write = Event::new(2_000, 42, Class::File, "write", "/home/test/Notes.txt");
        write.args = json!({"offset": 0, "length": 8});
        let mut connect = Event::new(3_000, 43, Class::Network, "connect", "93.184.216.34:80");
        connect.result = "ECONNREFUSED".into();
        let mut scan = Event::new(4_000, 7, Class::File, "open", "/home/test/Notes.txt");
        scan.outside = true;
        vec![open, write, connect, scan]
    }

    fn filled() -> Store {
        let mut store = Store::in_memory().unwrap();
        store.insert(&sample()).unwrap();
        store
    }

    #[test]
    fn events_come_back_as_they_went_in() {
        assert_eq!(filled().query(&Filter::default()).unwrap(), sample());
    }

    #[test]
    fn filters_by_class_pid_and_operation() {
        let store = filled();
        let files = Filter { classes: vec![Class::File], ..Filter::default() };
        assert_eq!(store.count(&files).unwrap(), 3);
        let by_pid = Filter { pid: Some(43), ..Filter::default() };
        assert_eq!(store.query(&by_pid).unwrap()[0].op, "connect");
        let writes = Filter { op: Some("write".into()), ..Filter::default() };
        assert_eq!(store.count(&writes).unwrap(), 1);
        let both = Filter { classes: vec![Class::File, Class::Network], ..Filter::default() };
        assert_eq!(store.count(&both).unwrap(), 4);
    }

    #[test]
    fn finds_text_without_regard_to_case() {
        let store = filled();
        let subject = Filter { subject_contains: Some("notes.TXT".into()), ..Filter::default() };
        assert_eq!(store.count(&subject).unwrap(), 3);
        let result = Filter { text: Some("econnrefused".into()), ..Filter::default() };
        assert_eq!(store.count(&result).unwrap(), 1);
        let argument = Filter { text: Some("\"length\":8".into()), ..Filter::default() };
        assert_eq!(store.count(&argument).unwrap(), 1);
    }

    #[test]
    fn a_wildcard_character_in_the_text_is_only_a_character() {
        let store = filled();
        let percent = Filter { subject_contains: Some("%".into()), ..Filter::default() };
        assert_eq!(store.count(&percent).unwrap(), 0);
    }

    #[test]
    fn filters_by_time_and_by_who_did_it() {
        let store = filled();
        let window = Filter { from_ns: Some(2_000), until_ns: Some(4_000), ..Filter::default() };
        assert_eq!(store.count(&window).unwrap(), 2);
        let outside = Filter { outside: Some(true), ..Filter::default() };
        assert_eq!(store.query(&outside).unwrap()[0].pid, 7);
    }

    #[test]
    fn counts_by_class_and_lists_what_was_touched() {
        let store = filled();
        assert_eq!(store.count_by_class().unwrap(), [("file".to_string(), 3), ("network".to_string(), 1)]);
        assert_eq!(store.subjects(Class::File, &["open", "write"], 10).unwrap(), [("/home/test/Notes.txt".to_string(), 3)]);
        assert_eq!(store.subjects(Class::File, &["write"], 10).unwrap(), [("/home/test/Notes.txt".to_string(), 1)]);
        assert_eq!(store.subjects(Class::Network, &["connect"], 10).unwrap(), [("93.184.216.34:80".to_string(), 1)]);
        assert_eq!(store.subjects(Class::File, &["delete"], 10).unwrap(), []);
    }

    #[test]
    fn pages_through_results() {
        let store = filled();
        let page = Filter { limit: Some(2), offset: 1, ..Filter::default() };
        let events = store.query(&page).unwrap();
        assert_eq!(events.iter().map(|event| event.time_ns).collect::<Vec<_>>(), vec![2_000, 3_000]);
        assert_eq!(store.count(&page).unwrap(), 4);
    }

    #[test]
    fn a_stored_file_can_be_opened_again() {
        let folder = std::env::temp_dir().join(format!("mc-core-test-{}", std::process::id()));
        std::fs::create_dir_all(&folder).unwrap();
        let path = folder.join("events.sqlite");
        {
            let mut store = Store::open(&path).unwrap();
            store.insert(&sample()).unwrap();
        }
        assert_eq!(Store::open(&path).unwrap().count(&Filter::default()).unwrap(), 4);
        std::fs::remove_dir_all(&folder).unwrap();
    }

    #[test]
    fn class_names_round_trip() {
        for class in Class::ALL {
            assert_eq!(Class::parse(class.as_str()), Some(class));
            assert_eq!(serde_json::to_value(class).unwrap(), json!(class.as_str()));
        }
    }
}
