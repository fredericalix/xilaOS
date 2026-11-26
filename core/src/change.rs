//! The SQL statements that change stored tables.
//!
//! ```text
//! CREATE TABLE name (column type [rule ...], ... [, CHECK (condition)])
//!     type: int, decimal, decimal(p,s), text
//!     rule: NOT NULL, UNIQUE, DEFAULT value, CHECK (condition)
//! DROP TABLE name
//! CREATE INDEX ON name (column)
//! DROP INDEX ON name (column)
//! INSERT INTO name [(column, ...)] VALUES (value, ...) [, (value, ...)]
//! UPDATE name SET column = value [, ...] [WHERE ...]
//! DELETE FROM name [WHERE ...]
//! ```
//!
//! A value is a number, a `'text'` or NULL. Every table has an `id` column
//! that the engine fills. A statement is one transaction: it is applied
//! whole or, if anything is wrong, not at all.
//!
//! What a column asks of its values is kept by the engine with the table.
//! The engine itself refuses a row without a value where NOT NULL asks
//! for one, and a value that another row has where UNIQUE says so: that
//! holds for whoever writes rows. The value by DEFAULT and the conditions
//! of CHECK are of this file: it gives the first to a row an INSERT does
//! not give a value, and works out the second for every row an INSERT or
//! an UPDATE writes. A condition that cannot be known, because a value
//! is missing, lets the row in.

use core::fmt::Write;

use xila_db::{Change, Database, Error, Rule, MAX_CHECK, MAX_CHECKS};

use crate::catalog::{self, stored_schema};
use crate::shell::Text;
use crate::expr::{self, Expr, Names, Vars, NO_VARS};
use crate::sql::{error, power_of_ten, row_from, unexpected, Field, Kind, Lexer, Message, Name, Schema, Token, Value, MAX_COLUMNS, MAX_SCALE};
use crate::text::FixedStr;

/// Rows one INSERT may add.
const MAX_INSERT: usize = 8;
/// Longest text a column takes for now: what a result table can show.
pub const MAX_TEXT: usize = 48;

/// What a statement did.
pub struct Done {
    /// What to tell the user.
    pub message: Message,
    /// The table it was about.
    pub table: Name,
    /// For a statement on the table itself: what happened to it.
    pub event: Option<&'static [u8]>,
}

pub(crate) fn engine_error(error: Error, schema: Option<&Schema>) -> Message {
    let text: &[u8] = match error {
        Error::Full => b"The database is full. Remove rows or tables to make room.",
        Error::Exists => b"A table of that name exists.",
        Error::TooManyTables => b"There are too many tables.",
        Error::ColumnCount => b"A table has one to seven columns, besides id.",
        Error::BadName => b"A name is a letter, then letters, digits or _; 24 at most.",
        Error::DuplicateColumn => b"Two columns have the same name. Every table already has id.",
        Error::Key => b"id is given by the system and cannot be changed.",
        Error::TooLong => b"That text is too long.",
        Error::Busy => b"The database is busy with another change.",
        Error::Disk => b"The disk does not answer. Nothing was changed.",
        Error::Stopped => b"A write to the disk failed earlier. Restart before changing anything.",
        Error::Foreign | Error::TooSmall => b"The disk cannot hold the database.",
        Error::Checks => b"A table keeps 4 checks at most, of 60 characters each.",
        Error::Missing(column) | Error::Twice(column, _) => {
            let mut message = Message::from_bytes(b"Column ");
            if let Some(schema) = schema {
                message.push_bytes(schema.columns[column.min(MAX_COLUMNS - 1)].bytes());
            }
            match error {
                Error::Twice(_, id) => {
                    let _ = write!(message, " takes no value twice: row {id} has that one.");
                }
                _ => message.push_bytes(b" must have a value."),
            }
            return message;
        }
        Error::Damaged(page) => {
            let mut message = Message::new();
            let _ = write!(message, "The database is damaged: page {page} cannot be read.");
            return message;
        }
        Error::WrongKind(column) => {
            let mut message = Message::from_bytes(b"Wrong type of value for column ");
            if let Some(schema) = schema {
                message.push_bytes(schema.columns[column.min(MAX_COLUMNS - 1)].bytes());
            }
            return message;
        }
    };
    Message::from_bytes(text)
}

/// Finds a stored table, and says why not when it cannot be changed.
fn stored(db: &Database, name: &[u8]) -> Result<Schema, Message> {
    if catalog::find(name).is_some() {
        return Err(error(name, b" is a view of the system and cannot be changed."));
    }
    if catalog::kept_apart(name) {
        return Err(error(name, b" is kept apart by the system: SQL neither reads nor changes it."));
    }
    match db.find(name) {
        Some(index) => Ok(stored_schema(&db.tables()[index], index)),
        None => {
            let mut message = error(b"Unknown table: ", name);
            message.push_bytes(b". Type tables to list them.");
            Err(message)
        }
    }
}

/// Turns a value into a field of a column, or says why the column does
/// not take it.
pub(crate) fn field<'a>(schema: &Schema, column: usize, value: &'a Value) -> Result<Field<'a>, Message> {
    let name = schema.columns[column].bytes();
    let wrong = |what: &[u8]| {
        let mut message = error(b"Column ", name);
        message.push_bytes(what);
        message
    };
    let too_large = || Message::from_bytes(b"That number is too large.");
    match (schema.kinds[column], value) {
        (_, Value::Null) => Ok(Field::Null),
        (Kind::Text, Value::Text(text)) => Ok(Field::Text(text.bytes())),
        (Kind::Text, _) => Err(wrong(b" takes a 'text' between quotes.")),
        (Kind::Int, Value::Text(_)) => Err(wrong(b" takes a whole number.")),
        (Kind::Dec(_), Value::Text(_)) => Err(wrong(b" takes a number.")),
        (kind, _) => {
            // a number goes in when the column keeps all of its digits
            let (units, digits) = value.number().unwrap_or((0, 0));
            let scale = if let Kind::Dec(scale) = kind { scale } else { 0 };
            let units = if digits > scale {
                let cut = power_of_ten(digits - scale);
                if units as i128 % cut != 0 {
                    if scale == 0 && kind == Kind::Int {
                        return Err(wrong(b" takes a whole number."));
                    }
                    let mut message = wrong(b" keeps ");
                    let _ = write!(message, "{scale} digits after the point.");
                    return Err(message);
                }
                units as i128 / cut
            } else {
                (units as i128).checked_mul(power_of_ten(scale - digits)).ok_or_else(too_large)?
            };
            let units = i64::try_from(units).map_err(|_| too_large())?;
            Ok(if kind == Kind::Int { Field::Int(units) } else { Field::Dec(units) })
        }
    }
}

/// A value written alone: a number, a `'text'` or NULL.
fn constant(lexer: &mut Lexer) -> Result<Value, Message> {
    let start = lexer.at;
    let negative = lexer.symbol(b"-");
    match lexer.next() {
        Token::Number(_) | Token::Decimal(..) => {}
        Token::String(_) if !negative => {}
        Token::Word(word) if !negative && word.eq_ignore_ascii_case(b"null") => {}
        other => return Err(unexpected(b"a number, a 'text' or NULL", other)),
    }
    // read as any value is, so that it is the same value
    let mut alone = Lexer::new(&lexer.text[start..lexer.at]);
    expr::parse(&mut alone, Names::without(&NO_VARS, b"A value is expected, not the name "))?.eval(&[Value::Null; MAX_COLUMNS])
}

/// The condition of a CHECK, after its opening parenthesis: the text up
/// to the parenthesis that closes it. It is read as a condition once all
/// the columns are known.
fn enclosed<'a>(lexer: &mut Lexer<'a>) -> Result<&'a [u8], Message> {
    let (text, start) = (lexer.text, lexer.at);
    let (mut depth, mut at, mut quoted) = (1, start, false);
    while at < text.len() {
        match text[at] {
            // a quote written twice inside a text opens and closes nothing
            b'\'' => quoted = !quoted,
            b'(' if !quoted => depth += 1,
            b')' if !quoted => depth -= 1,
            _ => {}
        }
        if depth == 0 {
            break;
        }
        at += 1;
    }
    if depth != 0 {
        return Err(Message::from_bytes(b"A check is closed by a parenthesis: check (n > 0)."));
    }
    lexer.at = at + 1;
    let inside = text[start..at].trim_ascii();
    if inside.is_empty() || inside.len() > MAX_CHECK {
        let mut message = Message::new();
        let _ = write!(message, "A check is a condition of {MAX_CHECK} characters at most.");
        return Err(message);
    }
    Ok(inside)
}

/// The conditions of a table, read once for a statement that writes rows.
struct Checks {
    conditions: [Expr; MAX_CHECKS],
    texts: [FixedStr<MAX_CHECK>; MAX_CHECKS],
    count: usize,
}

impl Checks {
    fn of(db: &Database, schema: &Schema) -> Result<Checks, Message> {
        let mut checks = Checks { conditions: [Expr::NONE; MAX_CHECKS], texts: [FixedStr::new(); MAX_CHECKS], count: 0 };
        for text in db.tables()[schema.table - catalog::TABLES.len()].checks.iter().take(MAX_CHECKS) {
            checks.texts[checks.count] = FixedStr::from_bytes(text);
            checks.conditions[checks.count] = condition_of(text, schema)?;
            checks.count += 1;
        }
        Ok(checks)
    }

    /// Refuses a row one of the conditions is known not to hold for, and
    /// says which.
    fn hold(&self, row: &[Value]) -> Result<(), Message> {
        for (condition, text) in self.conditions[..self.count].iter().zip(&self.texts) {
            if condition.fails(row)? {
                return Err(error(b"Refused by a check: ", text.bytes()));
            }
        }
        Ok(())
    }
}

/// Reads the text of a check as a condition on the columns of its table.
fn condition_of(text: &[u8], schema: &Schema) -> Result<Expr, Message> {
    let mut alone = Lexer::new(text);
    let condition = expr::parse(&mut alone, Names::of(schema, &NO_VARS))?;
    alone.end()?;
    Ok(condition)
}

/// Takes `(condition)` after the word CHECK, and keeps the condition.
fn check<'a>(lexer: &mut Lexer<'a>, checks: &mut [&'a [u8]; MAX_CHECKS], count: &mut usize) -> Result<(), Message> {
    lexer.expect(b"(")?;
    let condition = enclosed(lexer)?;
    if *count == MAX_CHECKS {
        return Err(engine_error(Error::Checks, None));
    }
    checks[*count] = condition;
    *count += 1;
    Ok(())
}

fn create<'a>(db: &mut Database, lexer: &mut Lexer<'a>) -> Result<Done, Message> {
    let name = lexer.word(b"a name for the table")?;
    if catalog::find(name).is_some() || catalog::kept_apart(name) {
        return Err(error(name, b" is a table of the system. Pick another name."));
    }
    lexer.expect(b"(")?;
    // the table as it will be, to read the values by default and the checks
    let mut schema = Schema::new(0, name, true);
    schema.add(b"id", Kind::Int);
    let mut columns: [(&[u8], Kind, Rule); MAX_COLUMNS] = [(b"", Kind::Int, Rule { not_null: false, unique: false, default: None }); MAX_COLUMNS];
    let mut defaults: [Option<Value>; MAX_COLUMNS] = [None; MAX_COLUMNS];
    let mut count = 0;
    let mut checks: [&'a [u8]; MAX_CHECKS] = [b""; MAX_CHECKS];
    let mut check_count = 0;
    loop {
        // a condition among the columns: `check (...)`, where a column
        // called check would be followed by its type
        let before = lexer.at;
        if lexer.keyword(b"check") && lexer.peek() == Token::Symbol(b"(") {
            check(lexer, &mut checks, &mut check_count)?;
            if !lexer.symbol(b",") {
                break;
            }
            continue;
        }
        lexer.at = before;
        let column = lexer.word(b"a column name")?;
        let kind = lexer.word(b"a type: int, decimal or text")?;
        let mut lower = FixedStr::<12>::new();
        for c in kind {
            lower.push(c.to_ascii_lowercase());
        }
        let mut kind = match lower.bytes() {
            b"int" | b"integer" | b"bigint" => Kind::Int,
            b"decimal" | b"numeric" => Kind::Dec(2),
            b"text" | b"varchar" | b"char" => Kind::Text,
            _ => return Err(error(b"Unknown type: ", kind).also(b". Types are int, decimal and text.")),
        };
        // (precision) or (precision, scale); only the scale matters here
        if lexer.symbol(b"(") {
            let mut numbers = [0i64; 2];
            let mut given = 0;
            loop {
                match lexer.next() {
                    Token::Number(n) if given < 2 => numbers[given] = n,
                    other => return Err(unexpected(b"a number", other)),
                }
                given += 1;
                if !lexer.symbol(b",") {
                    break;
                }
            }
            lexer.expect(b")")?;
            if let Kind::Dec(_) = kind {
                let scale = if given == 2 { numbers[1] } else { 0 };
                if !(0..=MAX_SCALE as i64).contains(&scale) {
                    return Err(Message::from_bytes(b"A decimal keeps 0 to 9 digits after the point."));
                }
                kind = Kind::Dec(scale as u8);
            }
        }
        if count == MAX_COLUMNS - 1 {
            return Err(engine_error(Error::ColumnCount, None));
        }
        schema.add(column, kind);
        // what the column asks of its values, in any order
        let mut rule = Rule { not_null: false, unique: false, default: None };
        loop {
            if lexer.keyword(b"not") {
                if !lexer.keyword(b"null") {
                    return Err(unexpected(b"NULL", lexer.peek()));
                }
                rule.not_null = true;
            } else if lexer.keyword(b"unique") {
                rule.unique = true;
            } else if lexer.keyword(b"default") {
                defaults[count] = Some(constant(lexer)?);
            } else if lexer.keyword(b"check") {
                check(lexer, &mut checks, &mut check_count)?;
            } else if lexer.keyword(b"primary") {
                return Err(Message::from_bytes(b"Every table has id for its key, given by the system: no other is declared."));
            } else if !lexer.keyword(b"null") {
                break;
            }
        }
        columns[count] = (column, kind, rule);
        count += 1;
        if !lexer.symbol(b",") {
            break;
        }
    }
    lexer.expect(b")")?;
    lexer.end()?;
    if count == 0 {
        return Err(engine_error(Error::ColumnCount, None));
    }
    // a value by default is one the column takes
    for (at, default) in defaults[..count].iter().enumerate() {
        if let Some(default) = default {
            columns[at].2.default = Some(field(&schema, at + 1, default)?);
        }
    }
    // a check is a condition on the columns of this table, read now so
    // that what is wrong with it is said when the table is made
    for check in &checks[..check_count] {
        if condition_of(check, &schema)?.names(0) {
            return Err(Message::from_bytes(b"A check cannot name id: a row has it only once it is in the table."));
        }
    }
    let index = db.create_with(name, &columns[..count], &checks[..check_count]).map_err(|e| engine_error(e, Some(&schema)))?;
    let table = Name::from_bytes(&db.tables()[index].name);
    Ok(Done { message: error(b"Table ", table.bytes()).also(b" created."), table, event: Some(b"create") })
}

/// Writes the statement that would make a stored table as it is: its
/// columns, what they ask, its checks.
pub fn written(db: &Database, name: &[u8], out: &mut dyn crate::shell::Out) -> bool {
    let schema = match stored(db, name) {
        Ok(schema) => schema,
        Err(message) => {
            out.line(crate::shell::Style::Error, message.bytes());
            return false;
        }
    };
    let table = &db.tables()[schema.table - catalog::TABLES.len()];
    let mut line = FixedStr::<160>::from_bytes(b"create table ");
    line.push_bytes(&table.name);
    line.push_bytes(b" (");
    out.line(crate::shell::Style::Out, line.bytes());
    let items = table.columns.len() - 1 + table.checks.len();
    for item in 0..items {
        line.clear();
        line.push_bytes(b"    ");
        match table.columns.get(item + 1) {
            Some(column) => {
                line.push_bytes(&column.name);
                line.push(b' ');
                line.push_bytes(catalog::kind_name(column.kind).bytes());
                let mut asked = FixedStr::<96>::new();
                catalog::rules(column, &mut asked);
                line.push_bytes(if asked.is_empty() { b"" } else { b" " });
                line.push_bytes(asked.bytes());
            }
            None => {
                line.push_bytes(b"check (");
                line.push_bytes(&table.checks[item + 1 - table.columns.len()]);
                line.push(b')');
            }
        }
        line.push_bytes(if item + 1 < items { b"," } else { b"" });
        out.line(crate::shell::Style::Out, line.bytes());
    }
    out.line(crate::shell::Style::Out, b")");
    // the indexes that are not those of a column that takes no value twice
    for column in table.columns.iter().filter(|column| column.indexed() && !column.unique) {
        line.clear();
        line.push_bytes(b"create index on ");
        line.push_bytes(&table.name);
        line.push_bytes(b" (");
        line.push_bytes(&column.name);
        line.push(b')');
        out.line(crate::shell::Style::Out, line.bytes());
    }
    true
}

/// `CREATE INDEX ON table (column)` and `DROP INDEX ON table (column)`.
/// An index has no name of its own: a column has one, or has none.
fn index(db: &mut Database, lexer: &mut Lexer, make: bool) -> Result<Done, Message> {
    if !lexer.keyword(b"on") {
        return Err(match lexer.peek() {
            Token::Word(_) => Message::from_bytes(b"An index has no name: create index on t (c), drop index on t (c)."),
            other => unexpected(b"ON", other),
        });
    }
    let schema = stored(db, lexer.word(b"a table")?)?;
    lexer.expect(b"(")?;
    let column = schema.column(lexer.word(b"a column")?)?;
    if lexer.peek() == Token::Symbol(b",") {
        return Err(Message::from_bytes(b"An index is on one column."));
    }
    lexer.expect(b")")?;
    lexer.end()?;
    let table = schema.table - catalog::TABLES.len();
    let (unique, indexed) = (db.tables()[table].columns[column].unique, schema.indexed[column]);
    let mut message = Message::new();
    let place = |message: &mut Message| {
        let _ = write!(message, "{} ({})", schema.name, schema.columns[column]);
    };
    match (make, indexed) {
        _ if column == 0 => return Err(Message::from_bytes(b"id needs no index: the rows are kept in its order.")),
        (true, true) => {
            place(&mut message);
            message.push_bytes(b" has an index already.");
            return Err(message);
        }
        (false, false) => {
            place(&mut message);
            message.push_bytes(b" has no index.");
            return Err(message);
        }
        (false, true) if unique => {
            let _ = write!(message, "Column {} takes no value twice: its index stays with it.", schema.columns[column]);
            return Err(message);
        }
        (true, false) => {
            db.index(table, column).map_err(|e| engine_error(e, Some(&schema)))?;
            message.push_bytes(b"Index made on ");
            place(&mut message);
            let rows = db.tables()[table].rows();
            let _ = write!(message, ", from {rows} row{}.", if rows == 1 { "" } else { "s" });
        }
        (false, true) => {
            db.unindex(table, column).map_err(|e| engine_error(e, Some(&schema)))?;
            message.push_bytes(b"Index removed from ");
            place(&mut message);
            message.push(b'.');
        }
    }
    Ok(Done { message, table: schema.name, event: None })
}

fn drop(db: &mut Database, lexer: &mut Lexer) -> Result<Done, Message> {
    let schema = stored(db, lexer.word(b"a table")?)?;
    lexer.end()?;
    let index = schema.table - catalog::TABLES.len();
    let rows = db.tables()[index].rows();
    db.remove(index).map_err(|e| engine_error(e, None))?;
    let mut message = error(b"Table ", schema.name.bytes());
    let _ = write!(message, " removed, with its {rows} row{}.", if rows == 1 { "" } else { "s" });
    Ok(Done { message, table: schema.name, event: Some(b"drop") })
}

fn insert(db: &mut Database, lexer: &mut Lexer, vars: &Vars) -> Result<Done, Message> {
    if !lexer.keyword(b"into") {
        return Err(unexpected(b"INTO", lexer.peek()));
    }
    let schema = stored(db, lexer.word(b"a table")?)?;
    let index = schema.table - catalog::TABLES.len();
    // which column each value goes to; by default, the columns after id in order
    let mut targets = [0usize; MAX_COLUMNS];
    let mut target_count = 0;
    if lexer.symbol(b"(") {
        loop {
            let column = schema.column(lexer.word(b"a column")?)?;
            if column == 0 {
                return Err(engine_error(Error::Key, None));
            }
            if target_count == MAX_COLUMNS || targets[..target_count].contains(&column) {
                return Err(error(b"This column is given twice: ", schema.columns[column].bytes()));
            }
            targets[target_count] = column;
            target_count += 1;
            if !lexer.symbol(b",") {
                break;
            }
        }
        lexer.expect(b")")?;
    } else {
        for column in 1..schema.count {
            targets[target_count] = column;
            target_count += 1;
        }
    }
    if !lexer.keyword(b"values") {
        return Err(unexpected(b"VALUES", lexer.peek()));
    }
    // what a column that is given no value takes, and what every row must hold
    let mut kept = [Field::Null; MAX_COLUMNS];
    for (kept, column) in kept.iter_mut().zip(&db.tables()[index].columns) {
        *kept = column.default.as_ref().map_or(Field::Null, |default| default.field());
    }
    let defaults = row_from(&kept[..schema.count], &schema.kinds);
    let checks = Checks::of(db, &schema)?;
    let mut ids: Option<(u64, u64)> = None;
    let mut rows = 0;
    loop {
        if rows == MAX_INSERT {
            return Err(Message::from_bytes(b"One INSERT adds eight rows at most."));
        }
        lexer.expect(b"(")?;
        // a value may be a calculation, but there is no row to read from
        let mut values = [Value::Null; MAX_COLUMNS];
        let mut given = 0;
        loop {
            let value = expr::parse(lexer, Names::without(vars, b"A value is expected, not the name "))?.eval(&[Value::Null; MAX_COLUMNS])?;
            if given < target_count {
                values[given] = value;
            }
            given += 1;
            if !lexer.symbol(b",") {
                break;
            }
        }
        lexer.expect(b")")?;
        // the row as it will be, the id apart: what was given, and the
        // value by default where nothing was
        let mut row = defaults;
        for (value, target) in values.iter().zip(&targets[..target_count.min(given)]) {
            row[*target] = *value;
        }
        let mut fields = [Field::Null; MAX_COLUMNS];
        for column in 1..schema.count {
            fields[column - 1] = field(&schema, column, &row[column])?;
        }
        if given != target_count {
            let mut message = Message::new();
            let _ = write!(message, "{target_count} values are expected for each row, not {given}.");
            return Err(message);
        }
        if checks.count > 0 {
            // as the table will keep it: a number in the digits of its column
            let mut whole = [Field::Null; MAX_COLUMNS];
            whole[1..schema.count].copy_from_slice(&fields[..schema.count - 1]);
            checks.hold(&row_from(&whole[..schema.count], &schema.kinds))?;
        }
        let id = db.insert(index, &fields[..schema.count - 1]).map_err(|e| engine_error(e, Some(&schema)))?;
        ids = Some((ids.map_or(id, |(first, _)| first), id));
        rows += 1;
        if !lexer.symbol(b",") {
            break;
        }
    }
    lexer.end()?;
    let (first, last) = ids.unwrap_or((0, 0));
    let mut message = Message::new();
    if rows == 1 {
        let _ = write!(message, "1 row added to {}, with id {first}.", schema.name);
    } else {
        let _ = write!(message, "{rows} rows added to {}, with ids {first} to {last}.", schema.name);
    }
    Ok(Done { message, table: schema.name, event: None })
}

fn count_message(count: usize, what: &str, table: &Name) -> Message {
    let mut message = Message::new();
    let _ = write!(message, "{count} row{} {what} {table}.", if count == 1 { "" } else { "s" });
    message
}

fn update(db: &mut Database, lexer: &mut Lexer, vars: &Vars) -> Result<Done, Message> {
    let schema = stored(db, lexer.word(b"a table")?)?;
    if !lexer.keyword(b"set") {
        return Err(unexpected(b"SET", lexer.peek()));
    }
    // what each column becomes: it may be worked out from the row
    let mut sets = [(0usize, Expr::NONE); MAX_COLUMNS];
    let mut count = 0;
    loop {
        let column = schema.column(lexer.word(b"a column")?)?;
        if column == 0 {
            return Err(engine_error(Error::Key, None));
        }
        lexer.expect(b"=")?;
        if count == MAX_COLUMNS {
            return Err(Message::from_bytes(b"Too many columns in SET."));
        }
        sets[count] = (column, expr::parse(lexer, Names::of(&schema, vars))?);
        count += 1;
        if !lexer.symbol(b",") {
            break;
        }
    }
    let filter = expr::condition(lexer, &schema, vars)?;
    lexer.end()?;
    let (kinds, sets) = (schema.kinds, &sets[..count]);
    let checks = Checks::of(db, &schema)?;
    let mut failed = None;
    // a condition on the id goes straight to those rows
    let Some(ids) = among(&filter) else {
        return Ok(Done { message: count_message(0, "changed in", &schema.name), table: schema.name, event: None });
    };
    let changed = db.update_each_ids(schema.table - catalog::TABLES.len(), ids, &mut |fields, put| {
        if failed.is_some() {
            return;
        }
        let row = row_from(fields, &kinds);
        let mut change = || -> Result<(), Message> {
            if let Some(filter) = &filter {
                if !filter.holds(&row)? {
                    return Ok(());
                }
            }
            let mut values = [Value::Null; MAX_COLUMNS];
            for (value, (_, expr)) in values.iter_mut().zip(sets) {
                *value = expr.eval(&row)?;
            }
            let mut new = [Field::Null; MAX_COLUMNS];
            new[..fields.len()].copy_from_slice(fields);
            for (value, (column, _)) in values.iter().zip(sets) {
                new[*column] = field(&schema, *column, value)?;
            }
            if checks.count > 0 {
                checks.hold(&row_from(&new[..fields.len()], &kinds))?;
            }
            put(&new[..fields.len()]);
            Ok(())
        };
        failed = change().err();
    });
    if let Some(message) = failed {
        return Err(message);
    }
    let changed = changed.map_err(|e| engine_error(e, Some(&schema)))?;
    Ok(Done { message: count_message(changed, "changed in", &schema.name), table: schema.name, event: None })
}

/// The ids a condition leaves a row to have: all of them without a
/// condition, None when no row can hold.
fn among(filter: &Option<Expr>) -> Option<xila_db::Ids> {
    match filter {
        Some(filter) => {
            let (first, last) = filter.bounds(0);
            catalog::ids(first, last)
        }
        None => Some(xila_db::Ids::ALL),
    }
}

fn delete(db: &mut Database, lexer: &mut Lexer, vars: &Vars) -> Result<Done, Message> {
    if !lexer.keyword(b"from") {
        return Err(unexpected(b"FROM", lexer.peek()));
    }
    let schema = stored(db, lexer.word(b"a table")?)?;
    let filter = expr::condition(lexer, &schema, vars)?;
    lexer.end()?;
    let kinds = schema.kinds;
    let mut failed = None;
    let Some(ids) = among(&filter) else {
        return Ok(Done { message: count_message(0, "removed from", &schema.name), table: schema.name, event: None });
    };
    let removed = db.delete_ids(schema.table - catalog::TABLES.len(), ids, &mut |fields| match &filter {
        _ if failed.is_some() => false,
        None => true,
        Some(filter) => filter.holds(&row_from(fields, &kinds)).unwrap_or_else(|message| {
            failed = Some(message);
            false
        }),
    });
    if let Some(message) = failed {
        return Err(message);
    }
    let removed = removed.map_err(|e| engine_error(e, None))?;
    Ok(Done { message: count_message(removed, "removed from", &schema.name), table: schema.name, event: None })
}

/// The table a statement that changes the database is about, when it
/// has the shape of one: for whoever must decide before it is run.
pub fn target(statement: &[u8]) -> Option<&[u8]> {
    let mut lexer = Lexer::new(statement);
    let verb = lexer.word(b"a statement").ok()?;
    let makes = verb.eq_ignore_ascii_case(b"create") || verb.eq_ignore_ascii_case(b"drop");
    let before: &[u8] = match verb.len() {
        // an index is a change to its table
        _ if makes && lexer.keyword(b"index") => b"on",
        6 if verb.eq_ignore_ascii_case(b"create") => b"table",
        4 if verb.eq_ignore_ascii_case(b"drop") => b"table",
        6 if verb.eq_ignore_ascii_case(b"insert") => b"into",
        6 if verb.eq_ignore_ascii_case(b"delete") => b"from",
        6 if verb.eq_ignore_ascii_case(b"update") => b"",
        _ => return None,
    };
    if !before.is_empty() && !lexer.keyword(before) {
        return None;
    }
    lexer.word(b"a table").ok()
}

/// Runs a statement that changes the database. `changed` is called for
/// every row the statement added, changed or removed, once it is certain:
/// the name of the table, what happened, the id of the row.
pub fn change(db: &mut Database, statement: &[u8], changed: &mut dyn FnMut(&[u8], Change, u64)) -> Result<Done, Message> {
    change_with(db, &NO_VARS, statement, changed)
}

/// The same, for a statement in which names may also be variables.
pub fn change_with(db: &mut Database, vars: &Vars, statement: &[u8], changed: &mut dyn FnMut(&[u8], Change, u64)) -> Result<Done, Message> {
    let mut lexer = Lexer::new(statement);
    let verb = lexer.word(b"a statement")?;
    let mut lower = FixedStr::<8>::new();
    for c in verb {
        lower.push(c.to_ascii_lowercase());
    }
    let table = |lexer: &mut Lexer| if lexer.keyword(b"table") { Ok(()) } else { Err(unexpected(b"TABLE", lexer.peek())) };
    let done = match lower.bytes() {
        b"create" if lexer.keyword(b"index") => index(db, &mut lexer, true),
        b"drop" if lexer.keyword(b"index") => index(db, &mut lexer, false),
        b"create" => table(&mut lexer).and_then(|_| create(db, &mut lexer)),
        b"drop" => table(&mut lexer).and_then(|_| drop(db, &mut lexer)),
        b"insert" => insert(db, &mut lexer, vars),
        b"update" => update(db, &mut lexer, vars),
        b"delete" => delete(db, &mut lexer, vars),
        _ => Err(error(b"Unknown statement: ", verb)),
    };
    match done {
        Ok(done) => {
            // a table that was just removed has nothing left to report
            let mut names = [Text::new(); xila_db::MAX_TABLES];
            for (name, table) in names.iter_mut().zip(db.tables()) {
                *name = Text::from_bytes(&table.name);
            }
            // the statement is done only once it is safe on the volume;
            // when that fails the engine has undone it
            db.commit(&mut |table, change, id| changed(names[table].bytes(), change, id)).map_err(|e| engine_error(e, None))?;
            Ok(done)
        }
        Err(message) => {
            db.rollback();
            Err(message)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::shell::System;
    use crate::sql::select;

    /// Runs statements; gives the message of the last one.
    fn run(sys: &mut System, statements: &[&str]) -> String {
        let mut last = String::new();
        for statement in statements {
            last = match change(&mut sys.db, statement.as_bytes(), &mut |_, _, _| {}) {
                Ok(done) => String::from_utf8_lossy(done.message.bytes()).into_owned(),
                Err(message) => format!("error: {}", String::from_utf8_lossy(message.bytes())),
            };
        }
        last
    }

    /// A query as lines of cells joined by `|`, without the header.
    fn rows(sys: &System, query: &str) -> Vec<String> {
        let table = select(sys, query.as_bytes()).unwrap_or_else(|m| panic!("{}", String::from_utf8_lossy(m.bytes())));
        (0..table.rows).map(|r| table.cells[r][..table.cols].iter().map(|c| String::from_utf8_lossy(c.bytes()).into_owned()).collect::<Vec<_>>().join("|")).collect()
    }

    fn books() -> System {
        let mut sys = System::new();
        assert_eq!(run(&mut sys, &["create table Expenses (day text, label text, amount decimal, tag text)"]), "Table expenses created.");
        assert_eq!(
            run(&mut sys, &["insert into expenses values ('2026-11-17', 'coffee', 3.5, 'food'), ('2026-11-17', 'train', 42, 'travel')"]),
            "2 rows added to expenses, with ids 1 to 2."
        );
        assert_eq!(run(&mut sys, &["insert into expenses (amount, label) values (-0.05, 'refund')"]), "1 row added to expenses, with id 3.");
        sys
    }

    fn parts() -> System {
        let mut sys = System::new();
        let made = "create table parts (label text not null unique, code int unique, count int default 1 check (count >= 0), \
                    price decimal(8,2) default 2.5 null, kind text default 'it''s', check (code is null or code > 99))";
        assert_eq!(run(&mut sys, &[made]), "Table parts created.");
        sys
    }

    #[test]
    fn a_row_takes_the_values_by_default_it_is_not_given() {
        let mut sys = parts();
        assert_eq!(run(&mut sys, &["insert into parts (label) values ('bolt')"]), "1 row added to parts, with id 1.");
        assert_eq!(rows(&sys, "select * from parts"), ["1|bolt||1|2.50|it's"]);
        // nothing, said in so many words, is nothing: not the value by default
        assert_eq!(run(&mut sys, &["insert into parts (label, count, kind) values ('nut', null, null)"]), "1 row added to parts, with id 2.");
        assert_eq!(run(&mut sys, &["insert into parts values ('pin', 100, 3, 0.5, 'small')"]), "1 row added to parts, with id 3.");
        assert_eq!(rows(&sys, "select * from parts where id > 1"), ["2|nut|||2.50|", "3|pin|100|3|0.50|small"]);
        // the view of the columns says what each asks
        assert_eq!(
            rows(&sys, "select name, type, rules from columns where table_name = 'parts'"),
            ["id|int|", "label|text|not null unique", "code|int|unique", "count|int|default 1", "price|decimal(18,2)|default 2.50", "kind|text|default 'it''s'"]
        );
        assert_eq!(rows(&sys, "select count(*) from columns where rules is not null"), ["5"]);
    }

    #[test]
    fn a_row_that_breaks_what_a_column_asks_is_refused() {
        let mut sys = parts();
        run(&mut sys, &["insert into parts (label, code) values ('bolt', 100), ('nut', 200), ('pin', null), ('rivet', null)"]);
        // a value where one is asked
        assert_eq!(run(&mut sys, &["insert into parts (code) values (300)"]), "error: Column label must have a value.");
        assert_eq!(run(&mut sys, &["insert into parts values (null, 300, 1, 1, 'x')"]), "error: Column label must have a value.");
        assert_eq!(run(&mut sys, &["update parts set label = null where id = 2"]), "error: Column label must have a value.");
        // no value twice; no value at all is not a value
        assert_eq!(run(&mut sys, &["insert into parts (label) values ('nut')"]), "error: Column label takes no value twice: row 2 has that one.");
        assert_eq!(run(&mut sys, &["insert into parts (label, code) values ('screw', 100)"]), "error: Column code takes no value twice: row 1 has that one.");
        assert_eq!(run(&mut sys, &["update parts set code = 200 where id = 3"]), "error: Column code takes no value twice: row 2 has that one.");
        assert_eq!(run(&mut sys, &["update parts set code = 500 where code is null"]), "error: Column code takes no value twice: row 3 has that one.");
        // one statement is whole or not at all: the first rows of this one are not kept
        assert_eq!(run(&mut sys, &["insert into parts (label) values ('a'), ('b'), ('a')"]), "error: Column label takes no value twice: row 5 has that one.");
        assert_eq!(rows(&sys, "select count(*), max(id) from parts"), ["4|4"]);
        // the checks, on what is added and on what is changed
        assert_eq!(run(&mut sys, &["insert into parts (label, count) values ('screw', -1)"]), "error: Refused by a check: count >= 0");
        assert_eq!(run(&mut sys, &["insert into parts (label, code) values ('screw', 5)"]), "error: Refused by a check: code is null or code > 99");
        assert_eq!(run(&mut sys, &["update parts set count = count - 2"]), "error: Refused by a check: count >= 0");
        assert_eq!(run(&mut sys, &["update parts set code = code - 50 where id = 1"]), "error: Refused by a check: code is null or code > 99");
        // a check that cannot be known lets the row in: count has no value here
        assert_eq!(run(&mut sys, &["update parts set count = null where id = 1"]), "1 row changed in parts.");
        // nothing above changed anything
        assert_eq!(rows(&sys, "select id, label, code, count from parts"), ["1|bolt|100|", "2|nut|200|1", "3|pin||1", "4|rivet||1"]);
        // what ends with no value twice is taken, whatever it goes through
        assert_eq!(run(&mut sys, &["update parts set code = code + 100"]), "4 rows changed in parts.");
        assert_eq!(run(&mut sys, &["update parts set label = 'bolt' where id = 1"]), "1 row changed in parts.");
        assert_eq!(rows(&sys, "select code from parts where code is not null"), ["200", "300"]);
        assert_eq!(sys.db.verify(), Ok(()));
    }

    #[test]
    fn what_is_wrong_in_what_a_table_asks_is_said_when_it_is_made() {
        let mut sys = System::new();
        let said = |sys: &mut System, columns: &str| run(sys, &[&format!("create table t ({columns})")]);
        assert_eq!(said(&mut sys, "a int default 'one'"), "error: Column a takes a whole number.");
        assert_eq!(said(&mut sys, "a int default 1.5"), "error: Column a takes a whole number.");
        assert_eq!(said(&mut sys, "a text default 3"), "error: Column a takes a 'text' between quotes.");
        assert_eq!(said(&mut sys, "a int default 1 + 1"), "error: Expected ), found: +");
        assert_eq!(said(&mut sys, "a int default"), "error: Expected a number, a 'text' or NULL, found: )");
        assert_eq!(said(&mut sys, "a int not"), "error: Expected NULL, found: )");
        assert_eq!(said(&mut sys, "a int primary key"), "error: Every table has id for its key, given by the system: no other is declared.");
        assert_eq!(said(&mut sys, "a int check (b > 0)"), "error: Unknown column: b");
        assert_eq!(said(&mut sys, "a int check (id > 0)"), "error: A check cannot name id: a row has it only once it is in the table.");
        assert_eq!(said(&mut sys, "a int check (a > 0 a)"), "error: Expected the end of the query, found: a");
        assert_eq!(said(&mut sys, "a int check ()"), "error: A check is a condition of 60 characters at most.");
        // the parenthesis that closes the check is then missing for the table
        assert_eq!(said(&mut sys, "a int check (a > 0"), "error: Expected ), but the query ends.");
        assert_eq!(run(&mut sys, &["create table t (a int check (a > 0"]), "error: A check is closed by a parenthesis: check (n > 0).");
        assert_eq!(said(&mut sys, &format!("a int check (a > {})", "0".repeat(57))), "error: A check is a condition of 60 characters at most.");
        assert_eq!(said(&mut sys, &["a int"; 1].iter().chain(&["check (a > 0)"; 5]).copied().collect::<Vec<_>>().join(", ")), "error: A table keeps 4 checks at most, of 60 characters each.");
        assert_eq!(said(&mut sys, "check (1 = 1)"), "error: A table has one to seven columns, besides id.");
        assert!(sys.db.tables().is_empty());
        // parentheses and quotes inside a text are part of it; a column may be called check
        assert_eq!(said(&mut sys, "a text default 'a (b' check (a <> ')' and a <> 'it''s ('), check int, n int NULL Not Null UNIQUE, check ((n + 1) * 2 > check)"), "Table t created.");
        assert_eq!(run(&mut sys, &["insert into t (check, n) values (5, 0)"]), "error: Refused by a check: (n + 1) * 2 > check");
        assert_eq!(run(&mut sys, &["insert into t (check, n) values (1, 1)"]), "1 row added to t, with id 1.");
        assert_eq!(run(&mut sys, &["update t set a = ')'"]), "error: Refused by a check: a <> ')' and a <> 'it''s ('");
        assert_eq!(rows(&sys, "select a, check, n from t"), ["a (b|1|1"]);
        // the statement that would make it again
        let mut out = crate::shell::tests::Lines::default();
        assert!(written(&sys.db, b"T", &mut out));
        assert_eq!(out.lines, ["create table t (", "    a text default 'a (b',", "    check int,", "    n int not null unique,", "    check (a <> ')' and a <> 'it''s ('),", "    check ((n + 1) * 2 > check)", ")"]);
        let mut out = crate::shell::tests::Lines::default();
        assert!(!written(&sys.db, b"tasks", &mut out) && !written(&sys.db, b"nowhere", &mut out));
        assert_eq!(out.lines, ["tasks is a view of the system and cannot be changed.", "Unknown table: nowhere. Type tables to list them."]);
    }

    /// Two thousand four hundred visits, to five towns, with a year that
    /// four of them share, a cost each and a few words: a table of some
    /// fifty pages.
    fn visits() -> System {
        let mut sys = System::new();
        run(&mut sys, &["create table visits (town text, year int, cost decimal(8,2), words text)"]);
        let towns = ["paris", "parma", "paris, texas", "oslo", "lima"];
        for batch in 0..300 {
            let rows: Vec<String> = (batch * 8..batch * 8 + 8).map(|n| format!("('{}', {}, {}, 'a few words about visit number {n:<6}')", towns[n % 5], 1990 + n % 600, n as f64 * 1.25)).collect();
            assert_eq!(run(&mut sys, &[&format!("insert into visits values {}", rows.join(", "))]).split(' ').next(), Some("8"));
        }
        sys
    }

    /// What a query gives, and the pages it read to give it.
    fn cost(sys: &System, query: &str) -> (Vec<String>, u64) {
        let before = sys.db.storage().looked;
        let found = rows(sys, query);
        (found, sys.db.storage().looked - before)
    }

    #[test]
    fn a_query_goes_to_its_rows_by_an_index_and_gives_the_same_ones() {
        let mut sys = visits();
        let queries = [
            "select id from visits where year = 2000",
            "select count(*), min(id), max(id) from visits where year between 2000 and 2002",
            "select id from visits where year in (1991, 2029) and town = 'oslo'",
            "select count(*) from visits where year > 2588",
            "select count(*) from visits where 1995 >= year",
            "select count(*) from visits where year > 5000",
            "select count(*) from visits where town = 'paris'",
            "select count(*) from visits where town = 'paris, texas' and cost > 100",
            "select count(*) from visits where town = 'nowhere'",
            "select id, town from visits where cost = 12.5",
            "select id from visits where cost = 100",
            "select count(*) from visits where cost = 12.505",
            "select count(*) from visits where year = null",
            "select count(*) from visits where year = 2000 or town = 'lima'",
        ];
        let whole: Vec<(Vec<String>, u64)> = queries.iter().map(|query| cost(&sys, query)).collect();
        assert_eq!(whole[0].0, ["11", "611", "1211", "1811"]);
        assert_eq!(whole[9].0, ["11|paris"]);
        assert_eq!(run(&mut sys, &["create index on visits (year)"]), "Index made on visits (year), from 2400 rows.");
        assert_eq!(run(&mut sys, &["create index on visits (town)"]), "Index made on visits (town), from 2400 rows.");
        assert_eq!(run(&mut sys, &["create index on visits (cost)"]), "Index made on visits (cost), from 2400 rows.");
        let by: Vec<(Vec<String>, u64)> = queries.iter().map(|query| cost(&sys, query)).collect();
        for ((query, whole), by) in queries.iter().zip(&whole).zip(&by) {
            assert_eq!(whole.0, by.0, "{query}");
        }
        // a value few rows have is found by reading a few pages, not the table
        for at in [0, 1, 3, 5, 8, 9, 10, 12] {
            assert!(by[at].1 * 3 < whole[at].1, "{}: {} pages by the index, {} without", queries[at], by[at].1, whole[at].1);
        }
        // what an index cannot tell reads the table as before: `or`, a value
        // the column does not keep; and so does a value that a fifth of
        // the rows have, or the years from the first to the last of a
        // list, give or take the pages that said so
        assert_eq!((by[11].1, by[13].1), (whole[11].1, whole[13].1));
        for at in [2, 4, 6, 7] {
            assert!(by[at].1 <= whole[at].1 + 4, "{}: {} pages by the index, {} without", queries[at], by[at].1, whole[at].1);
        }
        // a wrong comparison is still said
        let said = crate::sql::select(&sys, b"select id from visits where town = 5").err().map(|message| String::from_utf8_lossy(message.bytes()).into_owned());
        assert_eq!(said.as_deref(), Some("A number and a text cannot be compared."));
        // the indexes follow what changes
        assert_eq!(run(&mut sys, &["update visits set year = 1, town = 'rome' where id = 11"]), "1 row changed in visits.");
        assert_eq!(run(&mut sys, &["delete from visits where id = 611"]), "1 row removed from visits.");
        assert_eq!(run(&mut sys, &["insert into visits values ('rome', 2000, 1, null)"]), "1 row added to visits, with id 2401.");
        assert_eq!(rows(&sys, "select id from visits where year = 2000"), ["1211", "1811", "2401"]);
        assert_eq!(rows(&sys, "select id, year from visits where town = 'rome'"), ["11|1", "2401|2000"]);
        assert_eq!(sys.db.verify(), Ok(()));
        // a table says its indexes
        let mut out = crate::shell::tests::Lines::default();
        assert!(written(&sys.db, b"visits", &mut out));
        assert_eq!(out.lines[5..], [")", "create index on visits (town)", "create index on visits (year)", "create index on visits (cost)"]);
        assert_eq!(rows(&sys, "select name, index from columns where table_name = 'visits'"), ["id|", "town|yes", "year|yes", "cost|yes", "words|"]);
        // without it the query reads the table again, and says the same
        assert_eq!(run(&mut sys, &["drop index on visits (year)"]), "Index removed from visits (year).");
        let (found, read) = cost(&sys, "select count(*) from visits where year > 5000");
        assert_eq!((found, read), (vec!["0".to_string()], whole[5].1));
    }

    #[test]
    fn a_join_finds_the_rows_beside_by_an_index() {
        let mut sys = visits();
        run(&mut sys, &["create table towns (name text unique, people int)"]);
        run(&mut sys, &["insert into towns values ('paris', 2100), ('parma', 190), ('oslo', 700), ('lima', 9700), ('rome', 2800)"]);
        // what was bought on some of the visits: the visit it belongs to is a number in a column
        run(&mut sys, &["create table bought (visit int, thing text)"]);
        for batch in 0..100 {
            let rows: Vec<String> = (batch * 8..batch * 8 + 8).map(|n| format!("({}, 'thing number {n:<30}')", 1 + n * 3)).collect();
            run(&mut sys, &[&format!("insert into bought values {}", rows.join(", "))]);
        }
        let queries = [
            "select visits.id, bought.thing from visits join bought on bought.visit = visits.id where visits.id between 100 and 120",
            "select count(*) from visits left join bought on bought.visit = visits.id where visits.id < 40",
            "select towns.name, count(*) from towns join visits on visits.town = towns.name group by towns.name",
            "select towns.people, visits.id from visits join towns on towns.name = visits.town where visits.year = 2000",
        ];
        let whole: Vec<(Vec<String>, u64)> = queries.iter().map(|query| cost(&sys, query)).collect();
        assert_eq!(whole[0].0.len(), 7);
        assert_eq!(whole[0].0[0], format!("100|thing number {:<30}", 33));
        assert_eq!(whole[1].0, ["39"]);
        assert_eq!(whole[2].0, ["paris|480", "parma|480", "oslo|480", "lima|480"]);
        assert_eq!(whole[3].0, ["2100|11", "2100|611", "2100|1211", "2100|1811"]);
        run(&mut sys, &["create index on bought (visit)", "create index on visits (town)", "create index on visits (year)"]);
        let by: Vec<(Vec<String>, u64)> = queries.iter().map(|query| cost(&sys, query)).collect();
        for ((query, whole), by) in queries.iter().zip(&whole).zip(&by) {
            assert_eq!(by.0, whole.0, "{query}");
        }
        // for each visit, what was bought on it: a few pages, not the table of what was bought
        assert!(by[0].1 * 3 < whole[0].1 && by[1].1 * 3 < whole[1].1, "{} and {} pages by the index, {} and {} without", by[0].1, by[1].1, whole[0].1, whole[1].1);
        // a town has a fifth of the visits: the table is read as it lies, as before
        assert!(by[2].1 <= whole[2].1 + 5 * 4, "{} pages by the index, {} without", by[2].1, whole[2].1);
        // the first table by the index of a year, the second by the one its unique name always had
        assert!(by[3].1 * 3 < whole[3].1, "{} pages by the index, {} without", by[3].1, whole[3].1);
    }

    #[test]
    fn what_is_wrong_with_an_index_is_said() {
        let mut sys = visits();
        run(&mut sys, &["create table towns (name text unique, people int)"]);
        assert_eq!(run(&mut sys, &["create index on visits (year)", "create index on visits (YEAR)"]), "error: visits (year) has an index already.");
        assert_eq!(run(&mut sys, &["drop index on visits (town)"]), "error: visits (town) has no index.");
        assert_eq!(run(&mut sys, &["create index on visits (id)"]), "error: id needs no index: the rows are kept in its order.");
        assert_eq!(run(&mut sys, &["create index on visits (nothing)"]), "error: Unknown column: nothing");
        assert_eq!(run(&mut sys, &["create index on nowhere (year)"]), "error: Unknown table: nowhere. Type tables to list them.");
        assert_eq!(run(&mut sys, &["create index on tasks (name)"]), "error: tasks is a view of the system and cannot be changed.");
        assert_eq!(run(&mut sys, &["create index on visits (year, town)"]), "error: An index is on one column.");
        assert_eq!(run(&mut sys, &["create index by_year on visits (year)"]), "error: An index has no name: create index on t (c), drop index on t (c).");
        assert_eq!(run(&mut sys, &["create index visits (year)"]), "error: An index has no name: create index on t (c), drop index on t (c).");
        assert_eq!(run(&mut sys, &["create index on visits"]), "error: Expected (, but the query ends.");
        assert_eq!(run(&mut sys, &["create index"]), "error: Expected ON, but the query ends.");
        // a column that takes no value twice has its index from the start, and keeps it
        assert_eq!(run(&mut sys, &["create index on towns (name)"]), "error: towns (name) has an index already.");
        assert_eq!(run(&mut sys, &["drop index on towns (name)"]), "error: Column name takes no value twice: its index stays with it.");
        assert_eq!(rows(&sys, "select name, rules, index from columns where table_name = 'towns'"), ["id||", "name|unique|yes", "people||"]);
    }

    #[test]
    fn the_table_of_a_statement_is_known_before_it_is_run() {
        let of = |statement: &str| target(statement.as_bytes()).map(|name| String::from_utf8_lossy(name).into_owned());
        assert_eq!(of("insert into notes values (1)").as_deref(), Some("notes"));
        assert_eq!(of("  INSERT  INTO Programs (name) values ('x')").as_deref(), Some("Programs"));
        assert_eq!(of("update acts set went = 'done'").as_deref(), Some("acts"));
        assert_eq!(of("delete from acts where id = 1").as_deref(), Some("acts"));
        assert_eq!(of("drop table companions").as_deref(), Some("companions"));
        assert_eq!(of("create table acts (a int)").as_deref(), Some("acts"));
        assert_eq!(of("create index on acts (went)").as_deref(), Some("acts"));
        assert_eq!(of("DROP  INDEX ON Programs (name)").as_deref(), Some("Programs"));
        assert_eq!((of("create index acts"), of("drop index")), (None, None));
        assert_eq!((of("select * from acts"), of("drop acts"), of("insert notes"), of("")), (None, None, None, None));
    }

    #[test]
    fn tables_are_created_filled_and_read() {
        let sys = books();
        assert_eq!(rows(&sys, "select * from expenses"), ["1|2026-11-17|coffee|3.50|food", "2|2026-11-17|train|42.00|travel", "3||refund|-0.05|"]);
        assert_eq!(rows(&sys, "select sum(amount), count(day) from expenses"), ["45.45|2"]);
        assert_eq!(rows(&sys, "select label, amount from expenses where amount >= 3.5 order by amount desc"), ["train|42.00", "coffee|3.50"]);
        assert_eq!(rows(&sys, "select name, kind, columns, rows from tables where kind = 'stored'"), ["expenses|stored|5|3"]);
        assert_eq!(
            rows(&sys, "select position, name, type from columns where table_name = 'expenses'"),
            ["1|id|int", "2|day|text", "3|label|text", "4|amount|decimal(18,2)", "5|tag|text"]
        );
    }

    #[test]
    fn rows_are_changed_and_removed() {
        let mut sys = books();
        assert_eq!(run(&mut sys, &["update expenses set tag = 'misc', day = '2026-11-18' where tag is null"]), "1 row changed in expenses.");
        assert_eq!(run(&mut sys, &["update expenses set amount = 4 where label = 'coffee'"]), "1 row changed in expenses.");
        assert_eq!(rows(&sys, "select id, day, amount, tag from expenses where id != 2"), ["1|2026-11-17|4.00|food", "3|2026-11-18|-0.05|misc"]);
        assert_eq!(run(&mut sys, &["update expenses set tag = null"]), "3 rows changed in expenses.");
        assert_eq!(run(&mut sys, &["delete from expenses where amount < 10"]), "2 rows removed from expenses.");
        assert_eq!(rows(&sys, "select id, label from expenses"), ["2|train"]);
        assert_eq!(run(&mut sys, &["delete from expenses where id = 9"]), "0 rows removed from expenses.");
        assert_eq!(run(&mut sys, &["insert into expenses (label) values ('next')"]), "1 row added to expenses, with id 4.");
        assert_eq!(run(&mut sys, &["drop table expenses"]), "Table expenses removed, with its 2 rows.");
        assert_eq!(run(&mut sys, &["drop table expenses"]), "error: Unknown table: expenses. Type tables to list them.");
    }

    #[test]
    fn types_are_checked() {
        let mut sys = books();
        let mut check = |statement: &str, expected: &str| assert_eq!(run(&mut sys, &[statement]), expected, "{statement}");
        check("create table t (n int, price decimal(10,3), note varchar(20), whole decimal(5))", "Table t created.");
        check("insert into t values (1, 1.2345, 'x', 1)", "error: Column price keeps 3 digits after the point.");
        check("insert into t values (1.5, 1, 'x', 1)", "error: Column n takes a whole number.");
        check("insert into t values ('one', 1, 'x', 1)", "error: Column n takes a whole number.");
        check("insert into t values (1, 'one', 'x', 1)", "error: Column price takes a number.");
        check("insert into t values (1, 1, 2, 1)", "error: Column note takes a 'text' between quotes.");
        check("insert into t values (1, 1, 'x', 0.5)", "error: Column whole keeps 0 digits after the point.");
        check("insert into t values (1, 9999999999999999, 'x', 1)", "error: That number is too large.");
        check("insert into t values (1, 1, 'x')", "error: 4 values are expected for each row, not 3.");
        check("insert into t values (1, 1, 'x', 1, 1)", "error: 4 values are expected for each row, not 5.");
        check(
            "insert into t (note) values ('this text is longer than what a column holds for now')",
            "error: A text takes 48 characters at most, for now.",
        );
        check("insert into t (id) values (7)", "error: id is given by the system and cannot be changed.");
        check("update t set id = 7", "error: id is given by the system and cannot be changed.");
        check("insert into t (n, n) values (1, 2)", "error: This column is given twice: n");
        check("insert into t values (-3, -1.5, null, 12)", "1 row added to t, with id 1.");
        let sys = sys;
        assert_eq!(rows(&sys, "select * from t"), ["1|-3|-1.500||12"]);
    }

    #[test]
    fn values_can_be_worked_out() {
        let mut sys = books();
        // from nothing but numbers and texts, in INSERT
        assert_eq!(run(&mut sys, &["insert into expenses values ('2026-' || '11-19', upper('tea'), 3 * 1.25, null)"]), "1 row added to expenses, with id 4.");
        assert_eq!(rows(&sys, "select day, label, amount from expenses where id = 4"), ["2026-11-19|TEA|3.75"]);
        assert_eq!(run(&mut sys, &["insert into expenses (amount) values (amount + 1)"]), "error: A value is expected, not the name amount");
        assert_eq!(run(&mut sys, &["insert into expenses (amount) values (1 / 3)"]), "error: Column amount keeps 2 digits after the point.");
        assert_eq!(run(&mut sys, &["insert into expenses (amount) values (round(1 / 3, 2))"]), "1 row added to expenses, with id 5.");
        // from the row itself, in UPDATE
        assert_eq!(run(&mut sys, &["update expenses set amount = amount * 2, label = label || '!' where amount > 3 and amount < 4"]), "2 rows changed in expenses.");
        assert_eq!(rows(&sys, "select id, label, amount from expenses order by id"), ["1|coffee!|7.00", "2|train|42.00", "3|refund|-0.05", "4|TEA!|7.50", "5||0.33"]);
        assert_eq!(run(&mut sys, &["update expenses set amount = amount + 0.005"]), "error: Column amount keeps 2 digits after the point.");
        assert_eq!(run(&mut sys, &["update expenses set amount = amount / 0 where id = 1"]), "error: Division by zero.");
        assert_eq!(run(&mut sys, &["update expenses set tag = amount where id = 1"]), "error: Column tag takes a 'text' between quotes.");
        assert_eq!(run(&mut sys, &["update expenses set amount = label where id = 1"]), "error: Column amount takes a number.");
        // nothing of a statement that failed half way stays
        assert_eq!(rows(&sys, "select sum(amount) from expenses"), ["56.78"]);
        // conditions of every shape, in UPDATE and DELETE
        assert_eq!(run(&mut sys, &["delete from expenses where amount < 0 or (label like 't%' and not amount > 10)"]), "2 rows removed from expenses.");
        assert_eq!(rows(&sys, "select id from expenses"), ["1", "2", "5"]);
        assert_eq!(run(&mut sys, &["delete from expenses where label = 3"]), "error: A number and a text cannot be compared.");
        assert_eq!(rows(&sys, "select count(*) from expenses"), ["3"]);
    }

    #[test]
    fn whole_numbers_and_decimals_meet() {
        let mut sys = System::new();
        run(&mut sys, &["create table t (n int, price decimal(10,3))", "insert into t values (4, 1.5)"]);
        // a column of whole numbers takes a decimal only when nothing is lost
        assert_eq!(run(&mut sys, &["update t set n = n / 2"]), "1 row changed in t.");
        assert_eq!(run(&mut sys, &["update t set n = n / 4"]), "error: Column n takes a whole number.");
        assert_eq!(run(&mut sys, &["update t set n = price * 2, price = n"]), "1 row changed in t.");
        assert_eq!(rows(&sys, "select n, price from t"), ["3|2.000"]);
        assert_eq!(run(&mut sys, &["update t set price = 1.2345"]), "error: Column price keeps 3 digits after the point.");
        assert_eq!(run(&mut sys, &["update t set price = 1.2340, n = -n"]), "1 row changed in t.");
        assert_eq!(rows(&sys, "select n, price, n + price from t"), ["-3|1.234|-1.766"]);
    }

    #[test]
    fn a_statement_is_applied_whole_or_not_at_all() {
        let mut sys = books();
        assert_eq!(
            run(&mut sys, &["insert into expenses (label, amount) values ('a', 1), ('b', 2), ('c', 'three')"]),
            "error: Column amount takes a number."
        );
        assert_eq!(rows(&sys, "select count(*) from expenses"), ["3"]);
    }

    #[test]
    fn a_statement_that_does_not_fit_changes_nothing() {
        let mut sys = System::new();
        // sixteen pages fill up fast
        let small = xila_db::Volume::single(Box::new(xila_db::Memory::new(16)));
        sys.db = Database::open(small).ok().unwrap();
        run(&mut sys, &["create table notes (body text)"]);
        let row = "('a text as long as a column takes it, 48 letters')";
        let eight = format!("insert into notes values {}", [row; 8].join(", "));
        let mut added = 0;
        loop {
            let said = run(&mut sys, &[&eight]);
            if said.starts_with("error") {
                assert_eq!(said, "error: The database is full. Remove rows or tables to make room.");
                break;
            }
            added += 8;
            assert!(added < 5000, "the database never fills");
        }
        assert!(added > 100);
        assert_eq!(rows(&sys, "select count(*) from notes"), [added.to_string()]);
        assert_eq!(run(&mut sys, &["delete from notes where id > 8"]), format!("{} rows removed from notes.", added - 8));
        assert_eq!(run(&mut sys, &[&eight]), format!("8 rows added to notes, with ids {} to {}.", added + 1, added + 8));
    }

    /// A disk that works until it is told to stop.
    struct Fragile(xila_db::Memory, std::rc::Rc<std::cell::Cell<bool>>);

    impl xila_db::Disk for Fragile {
        fn name(&self) -> &'static str {
            "fragile"
        }
        fn pages(&self) -> u64 {
            self.0.pages()
        }
        fn read(&mut self, page: u64, into: &mut xila_db::Page) -> Result<(), xila_db::DiskError> {
            self.0.read(page, into)
        }
        fn write(&mut self, page: u64, from: &xila_db::Page) -> Result<(), xila_db::DiskError> {
            if self.1.get() {
                return Err(xila_db::DiskError);
            }
            self.0.write(page, from)
        }
        fn flush(&mut self) -> Result<(), xila_db::DiskError> {
            Ok(())
        }
    }

    #[test]
    fn a_disk_that_fails_is_said_and_changes_nothing() {
        let broken = std::rc::Rc::new(std::cell::Cell::new(false));
        let disk = Fragile(xila_db::Memory::new(64), broken.clone());
        let mut sys = System::new();
        sys.db = Database::open(xila_db::Volume::single(Box::new(disk))).ok().unwrap();
        assert_eq!(String::from_utf8_lossy(catalog::kept(&sys).bytes()), "on disk, transaction 1");
        run(&mut sys, &["create table notes (body text)", "insert into notes values ('kept')"]);
        assert_eq!(String::from_utf8_lossy(catalog::kept(&sys).bytes()), "on disk, transaction 3");
        broken.set(true);
        let mut seen = 0;
        let said = change(&mut sys.db, b"insert into notes values ('lost')", &mut |_, _, _| seen += 1).err().unwrap();
        assert_eq!(String::from_utf8_lossy(said.bytes()), "The disk does not answer. Nothing was changed.");
        assert_eq!(seen, 0, "nothing is reported for a change that was not kept");
        assert_eq!(rows(&sys, "select body from notes"), ["kept"]);
        broken.set(false);
        assert_eq!(run(&mut sys, &["insert into notes values ('back')"]), "1 row added to notes, with id 3.");
        assert_eq!(rows(&sys, "select id, body from notes"), ["1|kept", "3|back"]);
    }

    #[test]
    fn changes_are_reported_once_certain() {
        let mut sys = books();
        let mut seen = Vec::new();
        let mut report = |table: &[u8], change: Change, id: u64| seen.push(format!("{} {change:?} {id}", String::from_utf8_lossy(table)));
        change(&mut sys.db, b"insert into expenses (label) values ('x'), ('y')", &mut report).ok().unwrap();
        change(&mut sys.db, b"update expenses set tag = 'z' where id = 4", &mut report).ok().unwrap();
        change(&mut sys.db, b"delete from expenses where id > 3", &mut report).ok().unwrap();
        assert!(change(&mut sys.db, b"insert into expenses (label) values ('x'), (3)", &mut report).is_err());
        assert_eq!(seen, ["expenses Insert 4", "expenses Insert 5", "expenses Update 4", "expenses Delete 4", "expenses Delete 5"]);
    }

    #[test]
    fn the_tables_of_the_system_are_protected() {
        let mut sys = books();
        let mut check = |statement: &str, expected: &str| assert_eq!(run(&mut sys, &[statement]), expected, "{statement}");
        check("create table tasks (x int)", "error: tasks is a table of the system. Pick another name.");
        check("delete from tasks", "error: tasks is a view of the system and cannot be changed.");
        check("insert into settings values ('a', 'b')", "error: settings is a view of the system and cannot be changed.");
        check("drop table tables", "error: tables is a view of the system and cannot be changed.");
        check("create table expenses (x int)", "error: A table of that name exists.");
        check("create table t (id int)", "error: Two columns have the same name. Every table already has id.");
        check("create table t (x blob)", "error: Unknown type: blob. Types are int, decimal and text.");
        check("create table t (a int, b int, c int, d int, e int, f int, g int, h int)", "error: A table has one to seven columns, besides id.");
        check("create t (x int)", "error: Expected TABLE, found: t");
        check("create table t (x int) please", "error: Expected the end of the query, found: please");
        check("update expenses set amount 3", "error: Expected =, found: 3");
        check("delete expenses", "error: Expected FROM, found: expenses");
        check("truncate expenses", "error: Unknown statement: truncate");
    }
}
