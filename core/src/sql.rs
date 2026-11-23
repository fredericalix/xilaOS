//! SQL: the query language of the system.
//!
//! This file reads: SELECT, over the tables a `Source` gives. Some are views
//! of the live state of the system (tasks, settings, memory, the city...),
//! the others store rows in the engine (`xila-db`). The statements that
//! change stored tables are in `change.rs`, and what stands for a value
//! (`amount * 2`, `n > 3 or tag is null`) is in `expr.rs`.
//!
//! ```text
//! SELECT * | expression [AS name], COUNT(*), SUM(expression), SUM(a) / COUNT(*), ...
//!   [FROM table]
//!   [WHERE condition]
//!   [GROUP BY column]
//!   [ORDER BY expression | name or position in the result [DESC]]
//!   [LIMIT n]
//! ```
//!
//! The functions over rows are COUNT, SUM, AVG, MIN and MAX. Without FROM
//! a SELECT is a calculation: `select 7 / 2`.
//!
//! Everything works in fixed size buffers, like the rest of this crate.

use core::cmp::Ordering;
use core::fmt::Write;

use crate::expr::{self, Aggregates, Binary, Expr, Names, Over, Vars, NO_VARS};
use crate::shell::{Table, Text, TABLE_COLS, TABLE_ROWS};
use crate::text::FixedStr;

pub use xila_db::{Field, Kind};

pub const MAX_COLUMNS: usize = TABLE_COLS;
/// Digits a decimal column may keep after the point.
pub const MAX_SCALE: u8 = 9;

/// Name of a table or of a column.
pub type Name = FixedStr<{ xila_db::MAX_NAME }>;

#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Value {
    Null,
    Int(i64),
    /// An exact decimal: a whole number of units, and how many digits of it
    /// come after the point. `Dec(1250, 2)` is 12.50.
    Dec(i64, u8),
    Text(Text),
}

pub(crate) fn power_of_ten(digits: u8) -> i128 {
    10i128.pow(digits.min(18) as u32)
}

/// Compares two numbers given as units and digits after the point.
pub(crate) fn compare_numbers(a: (i64, u8), b: (i64, u8)) -> Ordering {
    let scale = a.1.max(b.1);
    (a.0 as i128 * power_of_ten(scale - a.1)).cmp(&(b.0 as i128 * power_of_ten(scale - b.1)))
}

impl Value {
    pub fn text(bytes: &[u8]) -> Value {
        Value::Text(Text::from_bytes(bytes))
    }

    /// A number as units and digits after the point.
    pub(crate) fn number(&self) -> Option<(i64, u8)> {
        match *self {
            Value::Int(n) => Some((n, 0)),
            Value::Dec(units, scale) => Some((units, scale)),
            _ => None,
        }
    }

    /// Nothing, then numbers, then texts.
    fn compare(&self, other: &Value) -> Ordering {
        let rank = |value: &Value| match value {
            Value::Null => 0,
            Value::Int(_) | Value::Dec(..) => 1,
            Value::Text(_) => 2,
        };
        match (self, other) {
            (Value::Text(a), Value::Text(b)) => a.bytes().cmp(b.bytes()),
            _ => match (self.number(), other.number()) {
                (Some(a), Some(b)) => compare_numbers(a, b),
                _ => rank(self).cmp(&rank(other)),
            },
        }
    }

    pub fn shown(&self) -> Text {
        let mut text = Text::new();
        match *self {
            Value::Null => {}
            Value::Text(value) => text = value,
            Value::Int(n) => {
                let _ = write!(text, "{n}");
            }
            Value::Dec(units, scale) => {
                let unit = power_of_ten(scale);
                let (whole, part) = ((units as i128 / unit).unsigned_abs(), (units as i128 % unit).unsigned_abs());
                let sign = if units < 0 { "-" } else { "" };
                let _ = write!(text, "{sign}{whole}");
                if scale > 0 {
                    let _ = write!(text, ".{part:0width$}", width = scale as usize);
                }
            }
        }
        text
    }
}

pub type Row = [Value; MAX_COLUMNS];

/// A row of the engine as a row of values. `kinds` are those of the columns.
pub fn row_from(fields: &[Field], kinds: &[Kind]) -> Row {
    let mut row = [Value::Null; MAX_COLUMNS];
    for (i, field) in fields.iter().take(MAX_COLUMNS).enumerate() {
        row[i] = match (*field, kinds.get(i)) {
            (Field::Null, _) => Value::Null,
            (Field::Int(n), _) => Value::Int(n),
            (Field::Dec(units), Some(Kind::Dec(scale))) => Value::Dec(units, *scale),
            (Field::Dec(units), _) => Value::Int(units),
            (Field::Text(text), _) => Value::text(text),
        };
    }
    row
}

/// A table of the system as the catalog describes it. `kinds` has one
/// letter per column: `i` for a whole number, `t` for text.
pub struct TableDef {
    pub name: &'static [u8],
    pub columns: &'static [&'static [u8]],
    pub kinds: &'static [u8],
    pub about: &'static [u8],
}

/// What a query needs to know about a table.
#[derive(Clone, Copy)]
pub struct Schema {
    /// The number the source knows the table by.
    pub table: usize,
    pub name: Name,
    pub columns: [Name; MAX_COLUMNS],
    pub kinds: [Kind; MAX_COLUMNS],
    pub count: usize,
    /// Whether the table stores rows. The others are views of live state.
    pub stored: bool,
}

impl Schema {
    pub fn new(table: usize, name: &[u8], stored: bool) -> Schema {
        Schema { table, name: Name::from_bytes(name), columns: [Name::new(); MAX_COLUMNS], kinds: [Kind::Text; MAX_COLUMNS], count: 0, stored }
    }

    pub fn add(&mut self, name: &[u8], kind: Kind) {
        if self.count < MAX_COLUMNS {
            self.columns[self.count] = Name::from_bytes(name);
            self.kinds[self.count] = kind;
            self.count += 1;
        }
    }

    pub fn column(&self, name: &[u8]) -> Result<usize, Message> {
        self.columns[..self.count].iter().position(|c| c.bytes().eq_ignore_ascii_case(name)).ok_or_else(|| error(b"Unknown column: ", name))
    }
}

impl TableDef {
    pub fn schema(&self, table: usize) -> Schema {
        let mut schema = Schema::new(table, self.name, false);
        for (name, kind) in self.columns.iter().zip(self.kinds) {
            schema.add(name, if *kind == b'i' { Kind::Int } else { Kind::Text });
        }
        schema
    }
}

/// Where the rows come from.
pub trait Source {
    /// Describes a table, by name.
    fn schema(&self, name: &[u8]) -> Option<Schema>;
    /// Calls `visit` with every row of a table. Fails when the rows cannot
    /// be read, with what to tell the user.
    fn scan(&self, table: usize, visit: &mut dyn FnMut(&Row)) -> Result<(), Message>;
    /// As `scan`, when only the rows whose first column is a whole number
    /// from `first` to `last` are of use: the others may be left out, or
    /// not. A table that finds its rows by that column reads only those.
    fn scan_among(&self, table: usize, _first: i64, _last: i64, visit: &mut dyn FnMut(&Row)) -> Result<(), Message> {
        self.scan(table, visit)
    }
}

pub type Message = FixedStr<80>;

pub(crate) fn error(what: &[u8], detail: &[u8]) -> Message {
    let mut message = Message::from_bytes(what);
    message.push_bytes(detail);
    message
}

#[derive(Clone, Copy, PartialEq, Debug)]
pub(crate) enum Token<'a> {
    Word(&'a [u8]),
    Number(i64),
    /// A number written with a point: its units and the digits after the point.
    Decimal(i64, u8),
    String(&'a [u8]),
    Symbol(&'a [u8]),
    End,
}

pub(crate) struct Lexer<'a> {
    pub text: &'a [u8],
    pub at: usize,
}

impl<'a> Lexer<'a> {
    pub fn new(text: &'a [u8]) -> Lexer<'a> {
        Lexer { text, at: 0 }
    }

    pub fn next(&mut self) -> Token<'a> {
        let text = self.text;
        while self.at < text.len() && text[self.at] == b' ' {
            self.at += 1;
        }
        let start = self.at;
        let Some(&first) = text.get(start) else {
            return Token::End;
        };
        let word = |c: u8| c.is_ascii_alphanumeric() || c == b'_';
        // a sign is not part of a number: `n-1` is a subtraction
        if first.is_ascii_digit() {
            self.at += 1;
            while self.at < text.len() && text[self.at].is_ascii_digit() {
                self.at += 1;
            }
            let digits = core::str::from_utf8(&text[start..self.at]).unwrap_or("0");
            let whole: i64 = digits.parse().unwrap_or(i64::MAX);
            if text.get(self.at) != Some(&b'.') || !text.get(self.at + 1).is_some_and(|c| c.is_ascii_digit()) {
                return Token::Number(whole);
            }
            // the digits after the point; a number too large becomes the largest
            let (mut units, mut scale) = (whole as i128, 0u8);
            self.at += 1;
            while let Some(digit) = text.get(self.at).filter(|c| c.is_ascii_digit()) {
                let step = (digit - b'0') as i128;
                units = units.saturating_mul(10).saturating_add(step);
                scale = scale.saturating_add(1);
                self.at += 1;
            }
            return Token::Decimal(units.clamp(i64::MIN as i128, i64::MAX as i128) as i64, scale);
        }
        if word(first) {
            // a point between two parts of a name belongs to it: row.amount
            let part = |at: usize| text.get(at).is_some_and(|c| c.is_ascii_alphabetic() || *c == b'_');
            while self.at < text.len() && (word(text[self.at]) || (text[self.at] == b'.' && part(self.at + 1))) {
                self.at += 1;
            }
            return Token::Word(&text[start..self.at]);
        }
        if first == b'\'' {
            // a quote written twice is a quote inside the text, not its end
            let mut end = start + 1;
            while end < text.len() && (text[end] != b'\'' || text.get(end + 1) == Some(&b'\'')) {
                end += if text[end] == b'\'' { 2 } else { 1 };
            }
            let end = end.min(text.len());
            self.at = (end + 1).min(text.len());
            return Token::String(&text[start + 1..end]);
        }
        let two = matches!(text.get(start..start + 2), Some(b"<=" | b">=" | b"<>" | b"!=" | b"||"));
        self.at += if two { 2 } else { 1 };
        Token::Symbol(&text[start..self.at])
    }

    pub fn peek(&mut self) -> Token<'a> {
        let at = self.at;
        let token = self.next();
        self.at = at;
        token
    }

    /// Takes the next token if it is this keyword.
    pub fn keyword(&mut self, word: &[u8]) -> bool {
        match self.peek() {
            Token::Word(w) if w.eq_ignore_ascii_case(word) => {
                self.next();
                true
            }
            _ => false,
        }
    }

    pub fn symbol(&mut self, symbol: &[u8]) -> bool {
        if self.peek() == Token::Symbol(symbol) {
            self.next();
            true
        } else {
            false
        }
    }

    pub fn word(&mut self, expected: &[u8]) -> Result<&'a [u8], Message> {
        match self.next() {
            Token::Word(w) => Ok(w),
            other => Err(unexpected(expected, other)),
        }
    }

    /// Takes this symbol, or says that it was expected.
    pub fn expect(&mut self, symbol: &[u8]) -> Result<(), Message> {
        if self.symbol(symbol) {
            Ok(())
        } else {
            Err(unexpected(symbol, self.peek()))
        }
    }

    /// Nothing may follow.
    pub fn end(&mut self) -> Result<(), Message> {
        match self.peek() {
            Token::End => Ok(()),
            other => Err(unexpected(b"the end of the query", other)),
        }
    }
}

pub(crate) fn unexpected(expected: &[u8], found: Token) -> Message {
    let mut message = error(b"Expected ", expected);
    message.push_bytes(match found {
        Token::End => b", but the query ends.",
        _ => b", found: ",
    });
    match found {
        Token::Word(w) | Token::String(w) | Token::Symbol(w) => message.push_bytes(w),
        Token::Number(n) => {
            let _ = write!(message, "{n}");
        }
        Token::Decimal(units, scale) => message.push_bytes(Value::Dec(units, scale).shown().bytes()),
        Token::End => {}
    }
    message
}

/// An item of the list: worked out for each row of the table, or, when
/// it is made of functions over the rows, once for each group from a
/// row of their results.
#[derive(Clone, Copy)]
struct Item {
    expr: Expr,
    over: bool,
}

/// What the rows of the result are ordered by.
#[derive(Clone, Copy)]
enum Order {
    /// A column of the result.
    Result(usize),
    /// A value worked out for each row, which the result may not show.
    Key,
}

/// Matches text against a LIKE pattern: `%` is any run of characters, `_` any one.
pub(crate) fn like(text: &[u8], pattern: &[u8]) -> bool {
    match pattern.split_first() {
        None => text.is_empty(),
        Some((b'%', rest)) => (0..=text.len()).any(|skip| like(&text[skip..], rest)),
        Some((&p, rest)) => match text.split_first() {
            Some((&t, more)) => (p == b'_' || p.eq_ignore_ascii_case(&t)) && like(more, rest),
            None => false,
        },
    }
}

/// Whether what follows an item of ORDER BY ends it: then a name or a
/// number there is a column of the result, not the start of a calculation.
fn ends_order(token: Token) -> bool {
    match token {
        Token::End => true,
        Token::Word(word) => [&b"asc"[..], b"desc", b"limit"].iter().any(|end| end.eq_ignore_ascii_case(word)),
        _ => false,
    }
}

/// The result of a query, as values.
pub struct Rows {
    /// The table that was read; `result` when there was none.
    pub name: Name,
    pub headers: [Name; MAX_COLUMNS],
    pub cols: usize,
    pub rows: [[Value; MAX_COLUMNS]; TABLE_ROWS],
    pub count: usize,
    /// Rows that exist but did not fit.
    pub more: usize,
}

impl Rows {
    /// The result as a table of texts, ready to be shown.
    pub fn table(&self) -> Table {
        let mut header: [&[u8]; MAX_COLUMNS] = [b""; MAX_COLUMNS];
        for i in 0..self.cols {
            header[i] = self.headers[i].bytes();
        }
        let mut table = Table::new(self.name.bytes(), &header[..self.cols]);
        for row in self.rows.iter().take(self.count) {
            let mut cells = [Text::new(); MAX_COLUMNS];
            for i in 0..self.cols {
                cells[i] = row[i].shown();
            }
            let mut shown: [&[u8]; MAX_COLUMNS] = [b""; MAX_COLUMNS];
            for i in 0..self.cols {
                shown[i] = cells[i].bytes();
            }
            table.row(&shown[..self.cols]);
        }
        table.more = self.more;
        table
    }
}

/// Runs a SELECT and returns its result as a table.
pub fn select(source: &dyn Source, query: &[u8]) -> Result<Table, Message> {
    self::query(source, &NO_VARS, query).map(|rows| rows.table())
}

/// Runs a SELECT in which names may also be variables.
pub fn query(source: &dyn Source, vars: &Vars, query: &[u8]) -> Result<Rows, Message> {
    let mut lexer = Lexer::new(query);
    if !lexer.keyword(b"select") {
        return Err(Message::from_bytes(b"This is not a SELECT."));
    }

    // The list of what to show is kept as text until the table is known.
    let list_at = lexer.at;
    let mut schema = None;
    loop {
        match lexer.next() {
            Token::End => break,
            Token::Word(word) if word.eq_ignore_ascii_case(b"from") => {
                let table_name = lexer.word(b"a table")?;
                schema = Some(source.schema(table_name).ok_or_else(|| {
                    let mut message = error(b"Unknown table: ", table_name);
                    message.push_bytes(b". Type tables to list them.");
                    message
                })?);
                break;
            }
            _ => {}
        }
    }
    let after_table = lexer.at;
    let names = match &schema {
        Some(schema) => Names::of(schema, vars),
        None => Names::without(vars, b"Without FROM and a table there is no column "),
    };

    // Now the list.
    let mut items = [Item { expr: Expr::NONE, over: false }; MAX_COLUMNS];
    let mut aggregates = Aggregates::new();
    let mut headers = [Name::new(); MAX_COLUMNS];
    let mut count = 0;
    lexer.at = list_at;
    loop {
        let full = || Message::from_bytes(b"Too many columns: eight at most.");
        if lexer.symbol(b"*") {
            let Some(schema) = &schema else {
                return Err(Message::from_bytes(b"* stands for the columns of a table: add FROM and a table."));
            };
            for column in 0..schema.count {
                if count == MAX_COLUMNS {
                    return Err(full());
                }
                items[count].expr = Expr::of_column(column);
                headers[count] = schema.columns[column];
                count += 1;
            }
        } else {
            if count == MAX_COLUMNS {
                return Err(full());
            }
            let start = lexer.at;
            let item = &mut items[count];
            let mut header = Name::new();
            (item.expr, item.over) = expr::parse_item(&mut lexer, names, &mut aggregates)?;
            match (item.over, item.expr.column(), &schema) {
                // a function over the rows alone is named by its name
                (true, Some(function), _) => header.push_bytes(aggregates.list[function].function.name()),
                (false, Some(column), Some(schema)) => header = schema.columns[column],
                // a calculation is named by what was typed
                _ => {
                    for &byte in query[start..lexer.at].iter().filter(|byte| **byte != b' ') {
                        header.push(byte.to_ascii_lowercase());
                    }
                }
            }
            if lexer.keyword(b"as") {
                header = Name::from_bytes(lexer.word(b"a name")?);
            }
            headers[count] = header;
            count += 1;
        }
        if !lexer.symbol(b",") {
            break;
        }
    }
    match &schema {
        Some(_) if !lexer.keyword(b"from") => return Err(unexpected(b"FROM", lexer.peek())),
        Some(_) => lexer.at = after_table,
        None => {}
    }

    let filter = match &schema {
        Some(schema) => expr::condition(&mut lexer, schema, vars)?,
        None => None,
    };

    let mut group_by = None;
    if let (Some(schema), true) = (&schema, lexer.keyword(b"group")) {
        if !lexer.keyword(b"by") {
            return Err(unexpected(b"BY", lexer.peek()));
        }
        group_by = Some(schema.column(lexer.word(b"a column")?)?);
    }
    let items = &items[..count];
    let grouped = group_by.is_some() || items.iter().any(|item| item.over);
    let aggregates = &aggregates.list[..aggregates.count];
    if grouped {
        // what is not made of functions over the rows must be the column of the groups
        if let Some(i) = items.iter().position(|item| !item.over && (item.expr.column().is_none() || item.expr.column() != group_by)) {
            return Err(error(b"This must be in GROUP BY, or in a function: ", headers[i].bytes()));
        }
    }

    // ORDER BY takes a column of the result, by name or by position, or
    // anything that can be worked out from a row of the table.
    let mut order = None;
    let mut order_key = Expr::NONE;
    let mut descending = false;
    if lexer.keyword(b"order") {
        if !lexer.keyword(b"by") {
            return Err(unexpected(b"BY", lexer.peek()));
        }
        let at = lexer.at;
        let first = lexer.next();
        let of_result = match first {
            _ if !ends_order(lexer.peek()) => None,
            Token::Number(n) if n >= 1 && n as usize <= count => Some(n as usize - 1),
            Token::Word(word) => headers[..count].iter().position(|header| header.bytes().eq_ignore_ascii_case(word)),
            _ => None,
        };
        order = Some(match (of_result, &schema) {
            (Some(column), _) => Order::Result(column),
            (None, Some(_)) if !grouped => {
                lexer.at = at;
                order_key = expr::parse(&mut lexer, names)?;
                Order::Key
            }
            _ => match first {
                Token::Word(word) => return Err(error(b"ORDER BY needs a column of the result, not: ", word)),
                other => return Err(unexpected(b"a column of the result or its position", other)),
            },
        });
        descending = lexer.keyword(b"desc");
        if !descending {
            lexer.keyword(b"asc");
        }
    }

    let mut limit = TABLE_ROWS;
    // whoever asks for fewer rows than fit is not told that there are more
    let mut asked = false;
    if lexer.keyword(b"limit") {
        match lexer.next() {
            Token::Number(n) => {
                asked = n as usize <= TABLE_ROWS;
                limit = (n as usize).min(TABLE_ROWS);
            }
            other => return Err(unexpected(b"a number", other)),
        }
    }
    lexer.end()?;

    // Read the table. Without functions, `rows` holds the rows of the
    // result, kept in order as they come: when there are more than fit,
    // the first ones of the order stay. With functions over the rows it
    // holds one row per group, each cell being the running result of one
    // function, until the end turns it into the row of the result; `sorts`
    // then holds the value the group is for, and `counts` how many values
    // each function saw.
    let mut result = Rows {
        name: schema.as_ref().map_or(Name::from_bytes(b"result"), |schema| schema.name),
        headers,
        cols: count,
        rows: [[Value::Null; MAX_COLUMNS]; TABLE_ROWS],
        count: 0,
        more: 0,
    };
    let rows = &mut result.rows;
    let mut sorts = [Value::Null; TABLE_ROWS];
    let mut counts = [[0u32; MAX_COLUMNS]; TABLE_ROWS];
    let mut row_count = 0;
    let mut dropped = 0usize;
    let before = |a: &Value, b: &Value| a.compare(b) == if descending { Ordering::Greater } else { Ordering::Less };
    let mut take = |row: &Row| -> Result<(), Message> {
        if let Some(filter) = &filter {
            if !filter.holds(row)? {
                return Ok(());
            }
        }
        if !grouped {
            let mut made = [Value::Null; MAX_COLUMNS];
            for (cell, item) in made.iter_mut().zip(items) {
                *cell = item.expr.eval(row)?;
            }
            let sort = match order {
                Some(Order::Result(column)) => made[column],
                Some(Order::Key) => order_key.eval(row)?,
                None => Value::Null,
            };
            // its place: after the rows that are not behind it
            let mut place = row_count;
            while order.is_some() && place > 0 && before(&sort, &sorts[place - 1]) {
                place -= 1;
            }
            if place == TABLE_ROWS {
                dropped += 1;
                return Ok(());
            }
            if row_count == TABLE_ROWS {
                // the last one falls off
                dropped += 1;
            } else {
                row_count += 1;
            }
            rows.copy_within(place..row_count - 1, place + 1);
            sorts.copy_within(place..row_count - 1, place + 1);
            (rows[place], sorts[place]) = (made, sort);
            return Ok(());
        }
        let key = group_by.map_or(Value::Null, |column| row[column]);
        let group = match sorts[..row_count].iter().position(|group| *group == key) {
            Some(group) => group,
            None if row_count < TABLE_ROWS => {
                sorts[row_count] = key;
                row_count += 1;
                row_count - 1
            }
            None => {
                dropped += 1;
                return Ok(());
            }
        };
        for (i, item) in aggregates.iter().enumerate() {
            // as in SQL, the functions skip what is nothing; COUNT(*) counts rows
            let value = if item.every { Value::Int(1) } else { item.expr.eval(row)? };
            if value == Value::Null {
                continue;
            }
            counts[group][i] += 1;
            let result = &mut rows[group][i];
            match item.function {
                Over::Sum | Over::Avg => {
                    if value.number().is_none() {
                        return Err(Message::from_bytes(b"SUM and AVG take numbers."));
                    }
                    *result = if *result == Value::Null { value } else { expr::binary(Binary::Add, *result, value)? };
                }
                Over::Min if *result == Value::Null || value.compare(result) == Ordering::Less => *result = value,
                Over::Max if *result == Value::Null || value.compare(result) == Ordering::Greater => *result = value,
                _ => {}
            }
        }
        Ok(())
    };
    match &schema {
        Some(schema) => {
            let mut failed = None;
            // what the condition says of the first column, the `id` of a
            // stored table, lets the source go straight to those rows
            let (first, last) = filter.as_ref().map_or((i64::MIN, i64::MAX), |filter| filter.bounds(0));
            source.scan_among(schema.table, first, last, &mut |row: &Row| {
                if failed.is_none() {
                    failed = take(row).err();
                }
            })?;
            if let Some(message) = failed {
                return Err(message);
            }
        }
        None => take(&[Value::Null; MAX_COLUMNS])?,
    }

    if grouped {
        // without GROUP BY, functions give one row even for an empty table
        if group_by.is_none() && row_count == 0 {
            row_count = 1;
        }
        for group in 0..row_count {
            // what the functions found, then the items made of it
            let mut found = rows[group];
            for (i, item) in aggregates.iter().enumerate() {
                let seen = counts[group][i] as i64;
                found[i] = match item.function {
                    Over::Count => Value::Int(seen),
                    Over::Avg if seen > 0 => expr::binary(Binary::Divide, found[i], Value::Int(seen))?,
                    _ => found[i],
                };
            }
            let mut made = [Value::Null; MAX_COLUMNS];
            for (cell, item) in made.iter_mut().zip(items) {
                *cell = if item.over { item.expr.eval(&found)? } else { sorts[group] };
            }
            rows[group] = made;
        }
        if let Some(Order::Result(column)) = order {
            // insertion sort: small and keeps the order of equal rows
            for i in 1..row_count {
                let mut j = i;
                while j > 0 && before(&rows[j][column], &rows[j - 1][column]) {
                    rows.swap(j - 1, j);
                    j -= 1;
                }
            }
        }
    }

    result.count = row_count.min(limit);
    result.more = if asked { 0 } else { dropped };
    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Shop;

    const SHOP: [TableDef; 2] = [
        TableDef { name: b"fruit", columns: &[b"name", b"color", b"price", b"stock", b"kilo"], kinds: b"ttiii", about: b"what is for sale" },
        TableDef { name: b"empty", columns: &[b"x"], kinds: b"i", about: b"nothing" },
    ];

    impl Source for Shop {
        fn schema(&self, name: &[u8]) -> Option<Schema> {
            SHOP.iter().position(|t| t.name.eq_ignore_ascii_case(name)).map(|i| SHOP[i].schema(i))
        }

        fn scan(&self, table: usize, visit: &mut dyn FnMut(&Row)) -> Result<(), Message> {
            if table != 0 {
                return Ok(());
            }
            let fruit: [(&[u8], &[u8], i64, i64); 5] =
                [(b"apple", b"red", 30, 12), (b"banana", b"yellow", 20, 0), (b"cherry", b"red", 90, 5), (b"lemon", b"yellow", 25, 40), (b"plum", b"purple", 45, 7)];
            for (name, color, price, stock) in fruit {
                let mut row = [Value::Null; MAX_COLUMNS];
                row[0] = Value::text(name);
                row[1] = Value::text(color);
                row[2] = Value::Int(price);
                row[3] = Value::Int(stock);
                // the price of a kilo, in cents; nobody weighs the plums
                row[4] = if name == b"plum" { Value::Null } else { Value::Dec(price * 12 + 5, 2) };
                visit(&row);
            }
            Ok(())
        }
    }

    /// The result as lines of cells joined by `|`, header first.
    fn run(query: &str) -> Vec<String> {
        match select(&Shop, query.as_bytes()) {
            Ok(table) => {
                let text = |b: &[u8]| String::from_utf8_lossy(b).into_owned();
                let mut lines = vec![table.header[..table.cols].iter().map(|h| text(h.bytes())).collect::<Vec<_>>().join("|")];
                for r in 0..table.rows {
                    lines.push(table.cells[r][..table.cols].iter().map(|c| text(c.bytes())).collect::<Vec<_>>().join("|"));
                }
                lines
            }
            Err(message) => vec![format!("error: {}", String::from_utf8_lossy(message.bytes()))],
        }
    }

    #[test]
    fn star_and_columns() {
        assert_eq!(run("select * from fruit limit 2"), ["name|color|price|stock|kilo", "apple|red|30|12|3.65", "banana|yellow|20|0|2.45"]);
        assert_eq!(run("SELECT Price, name FROM Fruit LIMIT 1"), ["price|name", "30|apple"]);
    }

    #[test]
    fn where_with_and() {
        assert_eq!(run("select name from fruit where color = 'red' and price > 50"), ["name", "cherry"]);
        assert_eq!(run("select name from fruit where stock <= 5 and color <> 'red'"), ["name", "banana"]);
        assert_eq!(run("select name from fruit where name like '%an%'"), ["name", "banana"]);
        assert_eq!(run("select name from fruit where name like '_e%'"), ["name", "lemon"]);
        assert_eq!(run("select name from fruit where price = -1"), ["name"]);
    }

    #[test]
    fn order_and_limit() {
        assert_eq!(run("select name, price from fruit order by price desc limit 2"), ["name|price", "cherry|90", "plum|45"]);
        assert_eq!(run("select name from fruit order by 1 desc limit 1"), ["name", "plum"]);
        assert_eq!(run("select name, stock from fruit order by stock limit 1"), ["name|stock", "banana|0"]);
    }

    #[test]
    fn functions_and_groups() {
        assert_eq!(run("select count(*), sum(stock), min(price), max(price) from fruit"), ["count|sum|min|max", "5|64|20|90"]);
        assert_eq!(
            run("select color, count(*), sum(price) from fruit group by color order by sum desc"),
            ["color|count|sum", "red|2|120", "yellow|2|45", "purple|1|45"] // equal sums keep their order
        );
        assert_eq!(run("select count(*) from fruit where color = 'blue'"), ["count", "0"]);
        assert_eq!(run("select count(*) from empty"), ["count", "0"]);
        assert_eq!(run("select color from fruit group by color order by color"), ["color", "purple", "red", "yellow"]);
    }

    #[test]
    fn errors_say_what_is_wrong() {
        assert_eq!(run("select * from shoes"), ["error: Unknown table: shoes. Type tables to list them."]);
        assert_eq!(run("select taste from fruit"), ["error: Unknown column: taste"]);
        assert_eq!(run("select name, count(*) from fruit"), ["error: This must be in GROUP BY, or in a function: name"]);
        assert_eq!(run("select price * 2, count(*) from fruit group by price"), ["error: This must be in GROUP BY, or in a function: price*2"]);
        assert_eq!(run("select name from fruit where"), ["error: Expected a value, but the query ends."]);
        assert_eq!(run("select name from fruit where price ~ 3"), ["error: Expected the end of the query, found: ~"]);
        assert_eq!(run("select name from fruit limit 2 please"), ["error: Expected the end of the query, found: please"]);
        assert_eq!(run("select name from fruit limit -1"), ["error: Expected a number, found: -"]);
        assert_eq!(run("select median(price) from fruit"), ["error: Unknown function: median"]);
        assert_eq!(run("select sum(price) + price from fruit"), ["error: Next to a function over the rows, a column must be inside one."]);
        assert_eq!(run("select sum(count(*)) from fruit"), ["error: A function over the rows has no place here: count"]);
        assert_eq!(run("select name from fruit where sum(price) > 1"), ["error: A function over the rows has no place here: sum"]);
        assert_eq!(run("select name from fruit order by max(price)"), ["error: A function over the rows has no place here: max"]);
        assert_eq!(run("select sum(price) / (count(*) - 5) from fruit"), ["error: Division by zero."]);
        assert_eq!(run("select count(*)+count(*)+count(*)+count(*)+count(*)+count(*)+count(*)+count(*)+count(*) from fruit"), ["error: Too many functions over the rows: eight at most."]);
        assert_eq!(run("select name from fruit where price in (20, 25) and name not like 'b%' and stock between 30 and 50"), ["name", "lemon"]);
        assert_eq!(run("select sum(name) from fruit"), ["error: SUM and AVG take numbers."]);
        assert_eq!(run("select name fruit"), ["error: Without FROM and a table there is no column name"]);
        assert_eq!(run("select name from"), ["error: Expected a table, but the query ends."]);
        assert_eq!(run("select * "), ["error: * stands for the columns of a table: add FROM and a table."]);
        assert_eq!(run("select name from fruit where price / (stock - 12) > 1"), ["error: Division by zero."]);
        assert_eq!(run("select color, count(*) from fruit group by color order by price"), ["error: ORDER BY needs a column of the result, not: price"]);
        assert_eq!(run("delete from fruit"), ["error: This is not a SELECT."]);
    }

    #[test]
    fn decimals_are_exact() {
        assert_eq!(run("select name, kilo from fruit where kilo > 3.6 order by kilo desc"), ["name|kilo", "cherry|10.85", "apple|3.65"]);
        assert_eq!(run("select name from fruit where kilo = 2.450"), ["name", "banana"]);
        assert_eq!(run("select name from fruit where kilo < 3"), ["name", "banana"]);
        assert_eq!(run("select sum(kilo), min(kilo), max(kilo), count(kilo), count(*) from fruit"), ["sum|min|max|count|count", "20.00|2.45|10.85|4|5"]);
        assert_eq!(Value::Dec(-5, 2).shown().bytes(), b"-0.05");
        assert_eq!(Value::Dec(-1250, 2).shown().bytes(), b"-12.50");
        assert_eq!(Value::Dec(7, 0).shown().bytes(), b"7");
        assert_eq!(Lexer::new(b"0.05").next(), Token::Decimal(5, 2));
        assert_eq!(Lexer::new(b"-0.05").next(), Token::Symbol(b"-"));
        assert_eq!(Lexer::new(b"12.").next(), Token::Number(12));
        assert_eq!(run("select name from fruit where kilo < -0.05"), ["name"]);
    }

    #[test]
    fn calculations() {
        assert_eq!(run("select name, price * stock as worth from fruit where price * stock > 300"), ["name|worth", "apple|360", "cherry|450", "lemon|1000", "plum|315"]);
        assert_eq!(run("select name, price*2, kilo + 1 from fruit limit 1"), ["name|price*2|kilo+1", "apple|60|4.65"]);
        assert_eq!(run("select upper(name) || ' ' || color as label from fruit limit 2"), ["label", "APPLE red", "BANANA yellow"]);
        assert_eq!(run("select name from fruit where color = 'red' or stock = 0"), ["name", "apple", "banana", "cherry"]);
        assert_eq!(run("select name from fruit where (color = 'red' or color = 'yellow') and not price < 30"), ["name", "apple", "cherry"]);
        assert_eq!(run("select name from fruit where length(name) = 4"), ["name", "plum"]);
        assert_eq!(run("select name, stock > 5 as plenty from fruit limit 2"), ["name|plenty", "apple|1", "banana|0"]);
        // what is nothing stays nothing, and does not hold
        assert_eq!(run("select name, kilo * 2 from fruit where name like 'p%'"), ["name|kilo*2", "plum|"]);
        assert_eq!(run("select count(*) from fruit where kilo * 2 > 0 or kilo * 2 <= 0"), ["count", "4"]);
    }

    #[test]
    fn calculations_over_rows() {
        assert_eq!(run("select sum(price * stock), avg(price), avg(kilo) from fruit"), ["sum|avg|avg", "2125|42|5.00"]);
        assert_eq!(run("select avg(stock) as stock, max(name), min(color) from fruit"), ["stock|max|min", "12.8|plum|purple"]);
        assert_eq!(run("select color, avg(price) as mean from fruit group by color order by mean desc"), ["color|mean", "red|60", "purple|45", "yellow|22.5"]);
        assert_eq!(run("select count(kilo * 2), sum(kilo + 1) from fruit"), ["count|sum", "4|24.00"]);
        assert_eq!(run("select avg(price) from fruit where color = 'blue'"), ["avg", ""]);
        // a calculation made of such functions
        assert_eq!(run("select max(price) - 1 from fruit"), ["max(price)-1", "89"]);
        assert_eq!(run("select sum(price) / count(*) as mean, avg(price) from fruit"), ["mean|avg", "42|42"]);
        assert_eq!(run("select round(sum(kilo) / count(kilo), 1) as kilo, count(*) - count(kilo) as unknown from fruit"), ["kilo|unknown", "5.0|1"]);
        assert_eq!(run("select 'from ' || min(price) || ' to ' || max(price) as prices from fruit"), ["prices", "from 20 to 90"]);
        assert_eq!(
            run("select color, max(price) - min(price) as spread, sum(price * stock) / sum(stock) as mean from fruit group by color order by spread desc"),
            ["color|spread|mean", "red|60|47.647059", "yellow|5|25", "purple|0|45"]
        );
        // the same function twice is worked out twice, and the result may hold fewer columns than
        // functions; a name of column keeps 24 characters
        assert_eq!(run("select count(*) * count(*) + count(*) from fruit"), ["count(*)*count(*)+count(", "30"]);
        // over no row at all: nothing, which a calculation keeps
        assert_eq!(run("select sum(price) / count(*), count(*) + 1 from fruit where color = 'blue'"), ["sum(price)/count(*)|count(*)+1", "|1"]);
        assert_eq!(run("select coalesce(sum(x), 0) + 1 as n from empty"), ["n", "1"]);
    }

    #[test]
    fn a_select_without_a_table_is_a_calculation() {
        assert_eq!(run("select 7 / 2"), ["7/2", "3.5"]);
        assert_eq!(run("select 1 + 2 as three, 'a' || 'b', round(10 / 3, 2)"), ["three|'a'||'b'|round(10/3,2)", "3|ab|3.33"]);
        assert_eq!(run("select 0.1 + 0.2 = 0.3 as exact"), ["exact", "1"]);
        assert_eq!(run("select count(*)"), ["count", "1"]);
        assert_eq!(run("select 1 where 2 > 1"), ["error: Expected the end of the query, found: where"]);
    }

    #[test]
    fn order_by_anything() {
        // a column the result does not show, and a calculation
        assert_eq!(run("select name from fruit order by price desc limit 2"), ["name", "cherry", "plum"]);
        assert_eq!(run("select name from fruit order by price * stock desc limit 3"), ["name", "lemon", "cherry", "apple"]);
        assert_eq!(run("select name from fruit order by length(name), name limit 1"), ["error: Expected the end of the query, found: ,"]);
        assert_eq!(run("select name, price from fruit order by -price limit 1"), ["name|price", "cherry|90"]);
        // the name of a column of the result comes first, then its position
        assert_eq!(run("select name, stock as price from fruit order by price desc limit 1"), ["name|price", "lemon|40"]);
        assert_eq!(run("select name, stock from fruit order by 2 desc limit 1"), ["name|stock", "lemon|40"]);
        assert_eq!(run("select name from fruit order by 2 - stock limit 1"), ["name", "lemon"]);
        // nothing sorts first
        assert_eq!(run("select name from fruit order by kilo limit 2"), ["name", "plum", "banana"]);
    }

    /// A table with more rows than a result shows.
    struct Numbers;

    impl Source for Numbers {
        fn schema(&self, _: &[u8]) -> Option<Schema> {
            let mut schema = Schema::new(0, b"numbers", false);
            schema.add(b"n", Kind::Int);
            Some(schema)
        }

        fn scan(&self, _: usize, visit: &mut dyn FnMut(&Row)) -> Result<(), Message> {
            // 0, 37, 74, 11, ... every number below 100, once, out of order
            for i in 0..100 {
                let mut row = [Value::Null; MAX_COLUMNS];
                row[0] = Value::Int(i * 37 % 100);
                visit(&row);
            }
            Ok(())
        }
    }

    #[test]
    fn the_first_rows_of_the_order_are_kept_when_there_are_too_many() {
        let cells = |query: &str| {
            let table = select(&Numbers, query.as_bytes()).ok().unwrap();
            let rows: Vec<String> = (0..table.rows).map(|r| String::from_utf8_lossy(table.cells[r][0].bytes()).into_owned()).collect();
            (rows, table.more)
        };
        let (rows, more) = cells("select n from numbers order by n desc limit 3");
        assert_eq!((rows, more), (vec!["99".to_string(), "98".to_string(), "97".to_string()], 0));
        let (rows, more) = cells("select n from numbers order by n");
        assert_eq!(rows, (0..32).map(|n| n.to_string()).collect::<Vec<_>>());
        assert_eq!(more, 68);
        // rows that are equal in the order stay in the order they came
        let (rows, _) = cells("select n from numbers order by n % 10 desc limit 2");
        assert_eq!(rows, ["59", "29"]);
        // without an order, the first that come
        let (rows, more) = cells("select n from numbers limit 3");
        assert_eq!((rows, more), (vec!["0".to_string(), "37".to_string(), "74".to_string()], 0));
        assert_eq!(cells("select n from numbers limit 50").1, 68);
        let (rows, more) = cells("select max(n) from numbers");
        assert_eq!((rows, more), (vec!["99".to_string()], 0));
    }

    #[test]
    fn empty_values() {
        assert_eq!(run("select name from fruit where kilo is null"), ["name", "plum"]);
        assert_eq!(run("select count(*) from fruit where kilo is not null"), ["count", "4"]);
        assert_eq!(run("select sum(price), max(price) from fruit where color = 'blue'"), ["sum|max", "|"]);
        assert_eq!(run("select name from fruit where kilo is 3"), ["error: Expected NULL, found: 3"]);
    }

    #[test]
    fn like_patterns() {
        assert!(like(b"serial shell", b"serial%"));
        assert!(like(b"serial shell", b"%SHELL"));
        assert!(like(b"abc", b"a_c"));
        assert!(!like(b"abc", b"a_"));
        assert!(like(b"", b"%"));
    }
}
