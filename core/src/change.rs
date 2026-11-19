//! The SQL statements that change stored tables.
//!
//! ```text
//! CREATE TABLE name (column type, ...)      type: int, decimal, decimal(p,s), text
//! DROP TABLE name
//! INSERT INTO name [(column, ...)] VALUES (value, ...) [, (value, ...)]
//! UPDATE name SET column = value [, ...] [WHERE ...]
//! DELETE FROM name [WHERE ...]
//! ```
//!
//! A value is a number, a `'text'` or NULL. Every table has an `id` column
//! that the engine fills. A statement is one transaction: it is applied
//! whole or, if anything is wrong, not at all.

use core::fmt::Write;

use xila_db::{Change, Database, Error};

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
fn field<'a>(schema: &Schema, column: usize, value: &'a Value) -> Result<Field<'a>, Message> {
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

fn create(db: &mut Database, lexer: &mut Lexer) -> Result<Done, Message> {
    let name = lexer.word(b"a name for the table")?;
    if catalog::find(name).is_some() {
        return Err(error(name, b" is a table of the system. Pick another name."));
    }
    lexer.expect(b"(")?;
    let mut columns: [(&[u8], Kind); MAX_COLUMNS] = [(b"", Kind::Int); MAX_COLUMNS];
    let mut count = 0;
    loop {
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
        if count == MAX_COLUMNS {
            return Err(engine_error(Error::ColumnCount, None));
        }
        columns[count] = (column, kind);
        count += 1;
        if !lexer.symbol(b",") {
            break;
        }
    }
    lexer.expect(b")")?;
    lexer.end()?;
    let index = db.create(name, &columns[..count]).map_err(|e| engine_error(e, None))?;
    let table = Name::from_bytes(&db.tables()[index].name);
    Ok(Done { message: error(b"Table ", table.bytes()).also(b" created."), table, event: Some(b"create") })
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
        let mut fields = [Field::Null; MAX_COLUMNS];
        for (value, target) in values.iter().zip(&targets[..target_count.min(given)]) {
            fields[target - 1] = field(&schema, *target, value)?;
        }
        if given != target_count {
            let mut message = Message::new();
            let _ = write!(message, "{target_count} values are expected for each row, not {given}.");
            return Err(message);
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
    let mut failed = None;
    let changed = db.update_each(schema.table - catalog::TABLES.len(), &mut |fields, put| {
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

fn delete(db: &mut Database, lexer: &mut Lexer, vars: &Vars) -> Result<Done, Message> {
    if !lexer.keyword(b"from") {
        return Err(unexpected(b"FROM", lexer.peek()));
    }
    let schema = stored(db, lexer.word(b"a table")?)?;
    let filter = expr::condition(lexer, &schema, vars)?;
    lexer.end()?;
    let kinds = schema.kinds;
    let mut failed = None;
    let removed = db.delete(schema.table - catalog::TABLES.len(), &mut |fields| match &filter {
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
