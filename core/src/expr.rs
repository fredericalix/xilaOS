//! Calculations: what stands for a value in a query.
//!
//! ```text
//! amount * 2            price + tax           'Mr ' || name        -amount
//! amount > 10 and (tag = 'food' or tag is null)        not label like 'a%'
//! round(amount / 3, 2)  length(label)  upper(tag)  abs(n)  coalesce(tag, '-')
//! ```
//!
//! From what binds least to what binds most: `or`, `and`, `not`, the
//! comparisons (`= != <> < <= > >= like`, `is [not] null`), `+ - ||`,
//! `* / %`, the sign, then values, columns, functions and parentheses.
//!
//! How values behave:
//!
//! - Numbers are exact. Adding, subtracting and multiplying decimals keeps
//!   every digit (up to nine after the point). A number too large for the
//!   system is an error, never a wrong result.
//! - `/` divides as a calculator does: `7 / 2` is 3.5 and `6 / 2` is 3. It
//!   keeps up to six digits after the point, rounded, and drops the zeros
//!   at the end. `%` is the remainder of whole numbers.
//! - `||` joins texts, and so does `+` when one side is a text.
//! - Nothing (`null`) in a calculation or a comparison gives nothing. A
//!   condition that gives nothing does not hold.
//! - A condition is 1 when it holds and 0 when it does not.
//!
//! An expression is read once into a small tree, kept in a fixed array, and
//! then worked out for each row.

use core::cmp::Ordering;

use crate::change::MAX_TEXT;
use crate::shell::Text;
use crate::text::FixedStr;
use crate::sql::{compare_numbers, error, like, unexpected, Lexer, Message, Row, Schema, Token, Value, MAX_SCALE};

/// Values and operations in one expression.
const MAX_NODES: usize = 16;
/// How deep parentheses and signs may nest.
const MAX_DEPTH: usize = 12;
/// Digits after the point a division keeps at most.
const DIVISION_DIGITS: u8 = 6;

#[derive(Clone, Copy, PartialEq, Debug)]
pub(crate) enum Binary {
    Or,
    And,
    Equal,
    NotEqual,
    Less,
    LessOrEqual,
    Greater,
    GreaterOrEqual,
    Like,
    Add,
    Subtract,
    Join,
    Multiply,
    Divide,
    Remainder,
}

#[derive(Clone, Copy, PartialEq, Debug)]
enum Function {
    Length,
    Upper,
    Lower,
    Abs,
    Round,
    Coalesce,
}

/// Name, function, fewest and most arguments.
const FUNCTIONS: [(&[u8], Function, usize, usize); 6] = [
    (b"length", Function::Length, 1, 1),
    (b"upper", Function::Upper, 1, 1),
    (b"lower", Function::Lower, 1, 1),
    (b"abs", Function::Abs, 1, 1),
    (b"round", Function::Round, 1, 2),
    (b"coalesce", Function::Coalesce, 2, 2),
];

#[derive(Clone, Copy)]
enum Node {
    Value(Value),
    Column(usize),
    Negate(usize),
    Not(usize),
    /// Whether a value is nothing, or is something.
    IsNull(usize, bool),
    Binary(Binary, usize, usize),
    Call(Function, usize, Option<usize>),
}

/// An expression, ready to be worked out for a row.
#[derive(Clone, Copy)]
pub struct Expr {
    nodes: [Node; MAX_NODES],
    count: usize,
    root: usize,
}

fn too_large() -> Message {
    Message::from_bytes(b"That number is too large.")
}

fn boolean(holds: bool) -> Value {
    Value::Int(holds as i64)
}

/// A number from its units and its digits after the point.
fn number(units: i128, scale: u8) -> Result<Value, Message> {
    let units = i64::try_from(units).map_err(|_| too_large())?;
    Ok(if scale == 0 { Value::Int(units) } else { Value::Dec(units, scale) })
}

/// `units` with this many zeros more.
fn shifted(units: i64, digits: u8) -> Result<i128, Message> {
    10i128.checked_pow(digits as u32).and_then(|power| power.checked_mul(units as i128)).ok_or_else(too_large)
}

/// Divides and rounds to the nearest, halves away from zero.
fn divide_rounded(a: i128, b: i128) -> i128 {
    let (quotient, rest) = (a / b, a % b);
    if rest.unsigned_abs() * 2 >= b.unsigned_abs() {
        quotient + if (a < 0) == (b < 0) { 1 } else { -1 }
    } else {
        quotient
    }
}

/// Whether a value holds as a condition; `None` when it is nothing.
fn truth(value: &Value) -> Result<Option<bool>, Message> {
    match value {
        Value::Null => Ok(None),
        Value::Int(n) | Value::Dec(n, _) => Ok(Some(*n != 0)),
        Value::Text(_) => Err(Message::from_bytes(b"A condition holds or does not: a text cannot say.")),
    }
}

fn join(a: &Value, b: &Value) -> Result<Value, Message> {
    let (mut text, more) = (a.shown(), b.shown());
    if text.len() + more.len() > MAX_TEXT {
        return Err(Message::from_bytes(b"A text takes 48 characters at most, for now."));
    }
    text.push_bytes(more.bytes());
    Ok(Value::Text(text))
}

/// Works out an operation on two values. `and` and `or` are not here: they
/// may not need their second value.
pub(crate) fn binary(operation: Binary, a: Value, b: Value) -> Result<Value, Message> {
    use Binary::*;
    if a == Value::Null || b == Value::Null {
        return Ok(Value::Null);
    }
    let texts = matches!(a, Value::Text(_)) as u8 + matches!(b, Value::Text(_)) as u8;
    match operation {
        Join => return join(&a, &b),
        Add if texts > 0 => return join(&a, &b),
        Like => {
            return match (a, b) {
                (Value::Text(text), Value::Text(pattern)) => Ok(boolean(like(text.bytes(), pattern.bytes()))),
                _ => Err(Message::from_bytes(b"LIKE compares a text with a pattern.")),
            }
        }
        Equal | NotEqual | Less | LessOrEqual | Greater | GreaterOrEqual => {
            let order = match (a, b) {
                (Value::Text(a), Value::Text(b)) => a.bytes().cmp(b.bytes()),
                _ if texts > 0 => return Err(Message::from_bytes(b"A number and a text cannot be compared.")),
                _ => compare_numbers(a.number().unwrap_or((0, 0)), b.number().unwrap_or((0, 0))),
            };
            return Ok(boolean(match operation {
                Equal => order == Ordering::Equal,
                NotEqual => order != Ordering::Equal,
                Less => order == Ordering::Less,
                LessOrEqual => order != Ordering::Greater,
                Greater => order == Ordering::Greater,
                _ => order != Ordering::Less,
            }));
        }
        _ => {}
    }
    let (Some((x, x_scale)), Some((y, y_scale))) = (a.number(), b.number()) else {
        return Err(Message::from_bytes(b"A calculation takes numbers, not texts."));
    };
    let least = x_scale.max(y_scale);
    match operation {
        Add | Subtract => {
            let (x, y) = (shifted(x, least - x_scale)?, shifted(y, least - y_scale)?);
            number(if operation == Add { x + y } else { x - y }, least)
        }
        Multiply => {
            let (mut units, mut scale) = (x as i128 * y as i128, x_scale.saturating_add(y_scale));
            if scale > MAX_SCALE {
                units = divide_rounded(units, 10i128.checked_pow((scale - MAX_SCALE) as u32).ok_or_else(too_large)?);
                scale = MAX_SCALE;
            }
            number(units, scale)
        }
        Divide => {
            if y == 0 {
                return Err(Message::from_bytes(b"Division by zero."));
            }
            let digits = least.max(DIVISION_DIGITS);
            let mut units = divide_rounded(shifted(x, y_scale.saturating_add(digits))?, shifted(y, x_scale)?);
            let mut scale = digits;
            while scale > least && units % 10 == 0 {
                units /= 10;
                scale -= 1;
            }
            number(units, scale)
        }
        Remainder => {
            if x_scale != 0 || y_scale != 0 {
                return Err(Message::from_bytes(b"% takes whole numbers."));
            }
            if y == 0 {
                return Err(Message::from_bytes(b"Division by zero."));
            }
            number(x as i128 % y as i128, 0)
        }
        _ => Ok(Value::Null),
    }
}

fn call(function: Function, a: Value, b: Option<Value>) -> Result<Value, Message> {
    if function == Function::Coalesce {
        return Ok(if a == Value::Null { b.unwrap_or(Value::Null) } else { a });
    }
    if a == Value::Null {
        return Ok(Value::Null);
    }
    let needs_number = |name: &[u8]| error(name, b" takes a number.");
    match function {
        Function::Length => Ok(Value::Int(a.shown().len() as i64)),
        Function::Upper | Function::Lower => {
            let mut text = Text::new();
            for &byte in a.shown().bytes() {
                text.push(if function == Function::Upper { byte.to_ascii_uppercase() } else { byte.to_ascii_lowercase() });
            }
            Ok(Value::Text(text))
        }
        Function::Abs => {
            let (units, scale) = a.number().ok_or_else(|| needs_number(b"abs"))?;
            number((units as i128).abs(), scale)
        }
        Function::Round => {
            let (units, scale) = a.number().ok_or_else(|| needs_number(b"round"))?;
            let digits = match b {
                None => 0,
                Some(Value::Int(digits)) if (0..=MAX_SCALE as i64).contains(&digits) => digits as u8,
                Some(_) => return Err(Message::from_bytes(b"round keeps 0 to 9 digits after the point.")),
            };
            if digits >= scale {
                return Ok(a);
            }
            number(divide_rounded(units as i128, 10i128.checked_pow((scale - digits) as u32).ok_or_else(too_large)?), digits)
        }
        Function::Coalesce => Ok(a),
    }
}

impl Expr {
    /// An expression that is nothing.
    pub const NONE: Expr = Expr { nodes: [Node::Value(Value::Null); MAX_NODES], count: 1, root: 0 };

    /// The expression that is this column.
    pub fn of_column(column: usize) -> Expr {
        let mut expr = Expr::NONE;
        expr.nodes[0] = Node::Column(column);
        expr
    }

    /// The column, when the expression is nothing but one.
    pub fn column(&self) -> Option<usize> {
        match self.nodes[self.root] {
            Node::Column(column) => Some(column),
            _ => None,
        }
    }

    /// Works the expression out for a row.
    pub fn eval(&self, row: &Row) -> Result<Value, Message> {
        self.node(self.root, row)
    }

    /// Whether the expression holds for a row, as a condition.
    pub fn holds(&self, row: &Row) -> Result<bool, Message> {
        Ok(truth(&self.eval(row)?)? == Some(true))
    }

    fn node(&self, at: usize, row: &Row) -> Result<Value, Message> {
        Ok(match self.nodes[at] {
            Node::Value(value) => value,
            Node::Column(column) => row[column],
            Node::Negate(a) => binary(Binary::Subtract, Value::Int(0), self.node(a, row)?)?,
            Node::Not(a) => truth(&self.node(a, row)?)?.map_or(Value::Null, |holds| boolean(!holds)),
            Node::IsNull(a, wanted) => boolean((self.node(a, row)? == Value::Null) == wanted),
            // one side can settle these; the other is then not looked at
            Node::Binary(operation @ (Binary::And | Binary::Or), a, b) => {
                let settles = operation == Binary::Or;
                let first = truth(&self.node(a, row)?)?;
                if first == Some(settles) {
                    return Ok(boolean(settles));
                }
                match (first, truth(&self.node(b, row)?)?) {
                    (_, Some(second)) if second == settles => boolean(settles),
                    (Some(_), Some(_)) => boolean(!settles),
                    _ => Value::Null,
                }
            }
            Node::Binary(operation, a, b) => binary(operation, self.node(a, row)?, self.node(b, row)?)?,
            Node::Call(function, a, b) => {
                let second = match b {
                    Some(b) => Some(self.node(b, row)?),
                    None => None,
                };
                call(function, self.node(a, row)?, second)?
            }
        })
    }
}

/// Values kept under a name: what `let` makes, and the columns of the row
/// a `for` is at (`row.amount`).
pub const MAX_VARS: usize = 32;
/// Longest name of a variable.
pub const VAR_NAME: usize = 32;

pub struct Vars {
    names: [FixedStr<VAR_NAME>; MAX_VARS],
    values: [Value; MAX_VARS],
    count: usize,
    /// The moment the line being run was given, in seconds since the year
    /// 2000, when the machine tells the time: what the names `now` and
    /// `today` stand for, unless a variable has taken them.
    pub clock: Option<u64>,
}

/// No variable at all, for what runs outside of a session.
pub static NO_VARS: Vars = Vars::new();

impl Vars {
    pub const fn new() -> Vars {
        Vars { names: [FixedStr::new(); MAX_VARS], values: [Value::Null; MAX_VARS], count: 0, clock: None }
    }

    fn find(&self, name: &[u8]) -> Option<usize> {
        self.names[..self.count].iter().position(|kept| kept.bytes().eq_ignore_ascii_case(name))
    }

    pub fn get(&self, name: &[u8]) -> Option<Value> {
        if let Some(at) = self.find(name) {
            return Some(self.values[at]);
        }
        // the date and the time, written so that they sort as they follow
        let moment = crate::clock::Moment::at(self.clock?);
        if name.eq_ignore_ascii_case(b"now") {
            Some(Value::Text(moment.text()))
        } else if name.eq_ignore_ascii_case(b"today") {
            Some(Value::Text(moment.date()))
        } else {
            None
        }
    }

    /// Gives a name a value. False when the name is too long or there is no
    /// room for one more.
    pub fn set(&mut self, name: &[u8], value: Value) -> bool {
        if let Some(at) = self.find(name) {
            self.values[at] = value;
            return true;
        }
        if self.count == MAX_VARS || name.len() > VAR_NAME {
            return false;
        }
        self.names[self.count] = FixedStr::from_bytes(name);
        self.names[self.count].lower();
        self.values[self.count] = value;
        self.count += 1;
        true
    }

    /// The names and their values, in the order they were made.
    pub fn each(&self) -> impl Iterator<Item = (&[u8], &Value)> {
        self.names[..self.count].iter().map(|name| name.bytes()).zip(&self.values[..self.count])
    }
}

impl Default for Vars {
    fn default() -> Vars {
        Vars::new()
    }
}

/// What a name may be where an expression is read: a column of a table if
/// there is one, then a variable.
#[derive(Clone, Copy)]
pub(crate) struct Names<'a> {
    columns: Option<&'a Schema>,
    vars: &'a Vars,
    /// Without a table: what to say before a name that is unknown.
    unknown: &'static [u8],
}

impl<'a> Names<'a> {
    pub fn of(columns: &'a Schema, vars: &'a Vars) -> Names<'a> {
        Names { columns: Some(columns), vars, unknown: b"Unknown column: " }
    }

    pub fn without(vars: &'a Vars, unknown: &'static [u8]) -> Names<'a> {
        Names { columns: None, vars, unknown }
    }
}

struct Parser<'a, 'b> {
    lexer: &'b mut Lexer<'a>,
    names: Names<'b>,
    expr: Expr,
    depth: usize,
}

impl Parser<'_, '_> {
    fn add(&mut self, node: Node) -> Result<usize, Message> {
        if self.expr.count == MAX_NODES {
            return Err(Message::from_bytes(b"This calculation is too long."));
        }
        self.expr.nodes[self.expr.count] = node;
        self.expr.count += 1;
        Ok(self.expr.count - 1)
    }

    /// Reads operations of one strength, joined left to right. `next`
    /// reads what binds more, `operation` tells an operator of this strength.
    fn chain(&mut self, next: fn(&mut Self) -> Result<usize, Message>, operation: fn(Token) -> Option<Binary>) -> Result<usize, Message> {
        let mut left = next(self)?;
        while let Some(found) = operation(self.lexer.peek()) {
            self.lexer.next();
            let right = next(self)?;
            left = self.add(Node::Binary(found, left, right))?;
        }
        Ok(left)
    }

    fn either(&mut self) -> Result<usize, Message> {
        self.chain(Self::both, |token| matches!(token, Token::Word(w) if w.eq_ignore_ascii_case(b"or")).then_some(Binary::Or))
    }

    fn both(&mut self) -> Result<usize, Message> {
        self.chain(Self::negation, |token| matches!(token, Token::Word(w) if w.eq_ignore_ascii_case(b"and")).then_some(Binary::And))
    }

    fn negation(&mut self) -> Result<usize, Message> {
        if !self.lexer.keyword(b"not") {
            return self.comparison();
        }
        self.depth += 1;
        if self.depth > MAX_DEPTH {
            return Err(Message::from_bytes(b"This calculation is too long."));
        }
        let inner = self.negation()?;
        self.depth -= 1;
        self.add(Node::Not(inner))
    }

    fn comparison(&mut self) -> Result<usize, Message> {
        let left = self.sum()?;
        if self.lexer.keyword(b"is") {
            let wanted = !self.lexer.keyword(b"not");
            if !self.lexer.keyword(b"null") {
                return Err(unexpected(b"NULL", self.lexer.peek()));
            }
            return self.add(Node::IsNull(left, wanted));
        }
        let operation = match self.lexer.peek() {
            Token::Symbol(b"=") => Binary::Equal,
            Token::Symbol(b"!=" | b"<>") => Binary::NotEqual,
            Token::Symbol(b"<") => Binary::Less,
            Token::Symbol(b"<=") => Binary::LessOrEqual,
            Token::Symbol(b">") => Binary::Greater,
            Token::Symbol(b">=") => Binary::GreaterOrEqual,
            Token::Word(w) if w.eq_ignore_ascii_case(b"like") => Binary::Like,
            _ => return Ok(left),
        };
        self.lexer.next();
        let right = self.sum()?;
        self.add(Node::Binary(operation, left, right))
    }

    fn sum(&mut self) -> Result<usize, Message> {
        self.chain(Self::product, |token| match token {
            Token::Symbol(b"+") => Some(Binary::Add),
            Token::Symbol(b"-") => Some(Binary::Subtract),
            Token::Symbol(b"||") => Some(Binary::Join),
            _ => None,
        })
    }

    fn product(&mut self) -> Result<usize, Message> {
        self.chain(Self::signed, |token| match token {
            Token::Symbol(b"*") => Some(Binary::Multiply),
            Token::Symbol(b"/") => Some(Binary::Divide),
            Token::Symbol(b"%") => Some(Binary::Remainder),
            _ => None,
        })
    }

    fn signed(&mut self) -> Result<usize, Message> {
        if !self.lexer.symbol(b"-") {
            return self.value();
        }
        self.depth += 1;
        if self.depth > MAX_DEPTH {
            return Err(Message::from_bytes(b"This calculation is too long."));
        }
        let inner = self.signed()?;
        self.depth -= 1;
        // a number written with its sign is that number
        if inner + 1 == self.expr.count {
            if let Node::Value(value @ (Value::Int(_) | Value::Dec(..))) = self.expr.nodes[inner] {
                self.expr.nodes[inner] = Node::Value(binary(Binary::Subtract, Value::Int(0), value)?);
                return Ok(inner);
            }
        }
        self.add(Node::Negate(inner))
    }

    fn value(&mut self) -> Result<usize, Message> {
        match self.lexer.next() {
            Token::Number(n) => self.add(Node::Value(Value::Int(n))),
            Token::Decimal(units, scale) => self.add(Node::Value(Value::Dec(units, scale))),
            Token::String(text) if text.len() <= MAX_TEXT => self.add(Node::Value(Value::text(text))),
            Token::String(_) => Err(Message::from_bytes(b"A text takes 48 characters at most, for now.")),
            Token::Symbol(b"(") => {
                self.depth += 1;
                if self.depth > MAX_DEPTH {
                    return Err(Message::from_bytes(b"This calculation is too long."));
                }
                let inner = self.either()?;
                self.depth -= 1;
                self.lexer.expect(b")")?;
                Ok(inner)
            }
            Token::Word(word) if word.eq_ignore_ascii_case(b"null") => self.add(Node::Value(Value::Null)),
            Token::Word(word) if word.eq_ignore_ascii_case(b"true") => self.add(Node::Value(Value::Int(1))),
            Token::Word(word) if word.eq_ignore_ascii_case(b"false") => self.add(Node::Value(Value::Int(0))),
            Token::Word(word) if self.lexer.symbol(b"(") => {
                let Some(&(name, function, least, most)) = FUNCTIONS.iter().find(|(name, ..)| name.eq_ignore_ascii_case(word)) else {
                    return Err(error(b"Unknown function: ", word));
                };
                let first = self.either()?;
                let second = if self.lexer.symbol(b",") { Some(self.either()?) } else { None };
                self.lexer.expect(b")")?;
                if (1 + second.is_some() as usize) < least || (second.is_some() && most == 1) {
                    return Err(error(name, if least == 2 { b" takes two values." } else { b" takes one value." }));
                }
                self.add(Node::Call(function, first, second))
            }
            Token::Word(word) => {
                let column = self.names.columns.and_then(|schema| schema.column(word).ok());
                match (column, self.names.vars.get(word)) {
                    (Some(column), _) => self.add(Node::Column(column)),
                    (None, Some(value)) => self.add(Node::Value(value)),
                    (None, None) => Err(error(self.names.unknown, word)),
                }
            }
            other => Err(unexpected(b"a value", other)),
        }
    }
}

/// Reads an expression. It ends where something that cannot continue it
/// begins: the caller looks at what follows.
pub(crate) fn parse(lexer: &mut Lexer, names: Names) -> Result<Expr, Message> {
    let mut parser = Parser { lexer, names, expr: Expr::NONE, depth: 0 };
    parser.expr.count = 0;
    let root = parser.either()?;
    parser.expr.root = root;
    Ok(parser.expr)
}

/// Reads `WHERE condition` if it is there.
pub(crate) fn condition(lexer: &mut Lexer, schema: &Schema, vars: &Vars) -> Result<Option<Expr>, Message> {
    if lexer.keyword(b"where") {
        parse(lexer, Names::of(schema, vars)).map(Some)
    } else {
        Ok(None)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sql::{Kind, MAX_COLUMNS};

    /// Works out an expression over a row with `n` 7, `price` 12.50,
    /// `label` 'tea' and `empty` nothing.
    fn work(text: &str) -> String {
        let mut schema = Schema::new(0, b"t", true);
        schema.add(b"n", Kind::Int);
        schema.add(b"price", Kind::Dec(2));
        schema.add(b"label", Kind::Text);
        schema.add(b"empty", Kind::Int);
        let mut row = [Value::Null; MAX_COLUMNS];
        (row[0], row[1], row[2]) = (Value::Int(7), Value::Dec(1250, 2), Value::text(b"tea"));
        let mut vars = Vars::new();
        assert!(vars.set(b"Rate", Value::Dec(15, 1)) && vars.set(b"row.n", Value::Int(3)) && vars.set(b"label", Value::text(b"hidden")));
        let mut lexer = Lexer::new(text.as_bytes());
        let result = parse(&mut lexer, Names::of(&schema, &vars)).and_then(|expr| lexer.end().map(|_| expr)).and_then(|expr| expr.eval(&row));
        match result {
            Ok(Value::Null) => "null".to_string(),
            Ok(Value::Text(text)) => format!("'{}'", String::from_utf8_lossy(text.bytes())),
            Ok(value) => String::from_utf8_lossy(value.shown().bytes()).into_owned(),
            Err(message) => format!("error: {}", String::from_utf8_lossy(message.bytes())),
        }
    }

    #[test]
    fn whole_numbers() {
        assert_eq!(work("1 + 2 * 3"), "7");
        assert_eq!(work("(1 + 2) * 3"), "9");
        assert_eq!(work("10 - 4 - 3"), "3");
        assert_eq!(work("n * n - 1"), "48");
        assert_eq!(work("-n"), "-7");
        assert_eq!(work("- -n"), "7");
        assert_eq!(work("3 - -2"), "5");
        assert_eq!(work("n-1"), "6");
        assert_eq!(work("17 % 5"), "2");
        assert_eq!(work("-17 % 5"), "-2");
        assert_eq!(work("9223372036854775807 + 1"), "error: That number is too large.");
        assert_eq!(work("9223372036854775807 * 2"), "error: That number is too large.");
        assert_eq!(work("abs(-9223372036854775807 - 1)"), "error: That number is too large.");
    }

    #[test]
    fn decimals_stay_exact() {
        assert_eq!(work("0.1 + 0.2"), "0.3");
        assert_eq!(work("price + 1"), "13.50");
        assert_eq!(work("price * 2"), "25.00");
        assert_eq!(work("price * 1.5"), "18.750");
        assert_eq!(work("price - 12.5"), "0.00");
        assert_eq!(work("1.005 + 2.5"), "3.505");
        assert_eq!(work("-0.05"), "-0.05");
        assert_eq!(work("0.123456789 * 0.1"), "0.012345679");
        assert_eq!(work("round(price * 1.196, 2)"), "14.95");
        assert_eq!(work("round(2.5)"), "3");
        assert_eq!(work("round(-2.5)"), "-3");
        assert_eq!(work("round(2.4)"), "2");
        assert_eq!(work("round(price, 5)"), "12.50");
        assert_eq!(work("abs(-0.5)"), "0.5");
    }

    #[test]
    fn division_is_that_of_a_calculator() {
        assert_eq!(work("7 / 2"), "3.5");
        assert_eq!(work("6 / 2"), "3");
        assert_eq!(work("1 / 3"), "0.333333");
        assert_eq!(work("2 / 3"), "0.666667");
        assert_eq!(work("-1 / 3"), "-0.333333");
        assert_eq!(work("price / 4"), "3.125");
        assert_eq!(work("price / 5"), "2.50");
        assert_eq!(work("10 / 0.5"), "20.0");
        assert_eq!(work("n / 0"), "error: Division by zero.");
        assert_eq!(work("n % 0"), "error: Division by zero.");
        assert_eq!(work("price % 2"), "error: % takes whole numbers.");
    }

    #[test]
    fn texts() {
        assert_eq!(work("label || '!'"), "'tea!'");
        assert_eq!(work("'n = ' || n"), "'n = 7'");
        assert_eq!(work("'total: ' + price"), "'total: 12.50'");
        assert_eq!(work("n + ' cups'"), "'7 cups'");
        assert_eq!(work("upper(label) || lower('ABC')"), "'TEAabc'");
        assert_eq!(work("length(label) + length(price)"), "8");
        assert_eq!(work("label * 2"), "error: A calculation takes numbers, not texts.");
        assert_eq!(work("abs(label)"), "error: abs takes a number.");
        assert_eq!(work("'0123456789012345678901234' || '0123456789012345678901234'"), "error: A text takes 48 characters at most, for now.");
    }

    #[test]
    fn conditions() {
        assert_eq!(work("n > 5"), "1");
        assert_eq!(work("n > 5 and label = 'coffee'"), "0");
        assert_eq!(work("n > 5 and label = 'coffee' or price >= 12.5"), "1");
        assert_eq!(work("n > 5 and (label = 'coffee' or price > 12.5)"), "0");
        assert_eq!(work("not n > 5"), "0");
        assert_eq!(work("not (n = 1 or n = 2)"), "1");
        assert_eq!(work("n + 1 = 2 * 4"), "1");
        assert_eq!(work("price = 12.5"), "1");
        assert_eq!(work("n <> 7"), "0");
        assert_eq!(work("label like 'T%'"), "1");
        assert_eq!(work("label like '_e'"), "0");
        assert_eq!(work("true and not false"), "1");
        assert_eq!(work("label = 3"), "error: A number and a text cannot be compared.");
        assert_eq!(work("n like 'a'"), "error: LIKE compares a text with a pattern.");
        assert_eq!(work("label and n"), "error: A condition holds or does not: a text cannot say.");
    }

    #[test]
    fn nothing_gives_nothing() {
        assert_eq!(work("empty + 1"), "null");
        assert_eq!(work("empty = empty"), "null");
        assert_eq!(work("empty is null"), "1");
        assert_eq!(work("n is not null"), "1");
        assert_eq!(work("label || empty"), "null");
        assert_eq!(work("coalesce(empty, 5) + 1"), "6");
        assert_eq!(work("coalesce(n, 5)"), "7");
        assert_eq!(work("upper(empty)"), "null");
        // one side is enough to settle `and` and `or`
        assert_eq!(work("empty > 1 and n = 0"), "0");
        assert_eq!(work("empty > 1 and n = 7"), "null");
        assert_eq!(work("empty > 1 or n = 7"), "1");
        assert_eq!(work("not empty > 1"), "null");
        // and the side that is not needed is not worked out
        assert_eq!(work("n = 0 and 1 / 0 > 1"), "0");
        assert_eq!(work("n = 7 or 1 / 0 > 1"), "1");
    }

    #[test]
    fn what_is_wrong_is_said() {
        assert_eq!(work("n +"), "error: Expected a value, but the query ends.");
        assert_eq!(work("(n + 1"), "error: Expected ), but the query ends.");
        assert_eq!(work("n + 1)"), "error: Expected the end of the query, found: )");
        assert_eq!(work("taste * 2"), "error: Unknown column: taste");
        assert_eq!(work("sqrt(n)"), "error: Unknown function: sqrt");
        assert_eq!(work("round(n, 2, 3)"), "error: Expected ), found: ,");
        assert_eq!(work("coalesce(n)"), "error: coalesce takes two values.");
        assert_eq!(work("upper(label, 2)"), "error: upper takes one value.");
        assert_eq!(work("round(price, 12)"), "error: round keeps 0 to 9 digits after the point.");
        assert_eq!(work("n is 3"), "error: Expected NULL, found: 3");
        assert_eq!(work("1+1+1+1+1+1+1+1+1+1"), "error: This calculation is too long.");
        assert_eq!(work(&"(".repeat(20)), "error: This calculation is too long.");
        let mut lexer = Lexer::new(b"amount");
        let said = parse(&mut lexer, Names::without(&NO_VARS, b"A value is expected, not the name ")).err().unwrap();
        assert_eq!(said.bytes(), b"A value is expected, not the name amount");
    }

    #[test]
    fn names_are_columns_first_then_variables() {
        assert_eq!(work("n * rate"), "10.5");
        assert_eq!(work("RATE + row.n"), "4.5");
        // a column hides a variable of the same name
        assert_eq!(work("label"), "'tea'");
        let mut vars = Vars::new();
        for n in 0..MAX_VARS {
            assert!(vars.set(format!("v{n}").as_bytes(), Value::Int(n as i64)));
        }
        assert!(!vars.set(b"one_more", Value::Null));
        assert!(vars.set(b"V3", Value::Int(-3)));
        assert_eq!(vars.get(b"v3"), Some(Value::Int(-3)));
        assert!(!Vars::new().set(&[b'x'; VAR_NAME + 1], Value::Null));
        assert_eq!(vars.each().count(), MAX_VARS);
    }
}
