# Panels: the screens of applications

A *panel* is the screen of an application: a list of the rows of a
table, under labels that are chosen, on a virtual screen of its own. It
is described in a few lines, on a table, and the system gives the rest:
the pages, the line to find, the keys. It is what a display file and a
"work with" screen are on an AS/400, with less to write.

What is built is the list and the form (28 November 2026). Panels
opened from a row and programs on a key are the next steps of the plan
in [Status and roadmap](13-status-and-roadmap.md#application-screens-panels).

## At a glance

```
XilaOS|all:/>create table items (label text, place text, qty int)
XilaOS|all:/>panel stock on items
..>title 'The stock'
..>list label, place, qty
..>field place 'Where it is'
..>field qty 'In stock'
..>end
Panel stock kept: 4 lines.
XilaOS|all:/>stock
```

The last line opens it:

```
 The stock                                                              page 1
 Find: _

 Opt label           Where it is  In stock
  _  Oscilloscope    bench 2             1
  _  Soldering iron  bench 1             2
  _  Cat6 cable 5m   shelf B            12


 2=Change  4=Remove  5=Display
 F3=Exit  F5=Refresh  F6=Add  PgUp/PgDn=Pages  Enter=Go  Esc=Overview
```

`F6` there, or `2` before a row, shows the form:

```
 The stock                                                      row 3, changed

 label .......... : Cat6 cable 5m
 Where it is .... : shelf B
 In stock ....... : 12_


 Enter=Save  Tab=Next field  F4=Values  F12=Cancel  F3=Exit
```

## Describing a panel

`panel NAME on TABLE` opens a block, as `program` does, and `end`
closes it. The table is a stored one, and must be there.

| Line | What it says |
|---|---|
| `title 'text'` | The title of the screen, 32 characters at most. Without it, the name of the panel. |
| `list a, b, c` | The columns of the list, in their order: eight at most, which is every column a table has, `id` among them if wanted. Without it, every column but `id`. |
| `field column 'Label'` | What a column is called on this panel, in 32 characters at most: the list shows the label over it. One field a column, never `id`. |
| `field column 'Label' shown` | A column the form shows and does not let type: it is never written by the panel. |
| `field column 'Label' among 'a', 'b'` | The values the field proposes at `F4`: texts between quotes, numbers. |
| `field column 'Label' among select ...` | The same, read from a table when `F4` is pressed: the first column of the `select`. |
| `# ...` | A note. |

- **A panel is an object of the system as a program is.** Its lines are
  rows of the stored table `panels` (`name`, `line`, `text`), line 0
  saying which table it is on. `panels` lists them, `show panel NAME`
  gives one back as it can be typed again, `drop panel NAME` removes
  it. Typing one again replaces it.
- **What is wrong with its form is told at its `end`**, with the line,
  and it is then not kept: an unknown column, a line that is none of
  those above, a title that is no text. One that was kept under that
  name stays as it was.
- **A panel and a program do not share a name**, and neither is named
  as a command. `panel` is a word of the language as `program` is: no
  value and no program can be named so.
- **The companion may not change the table `panels`**, nor drop a
  panel: a later step lets a panel run programs at a key.
- **With no `field` line, every column but `id` is a field**, under its
  name. With some, the form has those and no other: a column without a
  field is not shown there, and takes its value by default when a row
  is added.
- **A panel is read again each time it opens**, on its table as it is
  then. If a listed column is gone, or the table, it says so and does
  not open.

## Using one

Its name, typed alone at the prompt of the screen, opens it and shows
it. A panel opens at the screen of the machine only, for who sits
there: at the serial port, at the prompt over the network, for the
companion, in a program that runs by itself and in a job nobody waits
for, its name answers `A panel opens at the screen of the machine:
stock.` A program that the prompt of the screen waits for may open
one.

In the list:

| Key | What it does |
|---|---|
| `PgDn`, `PgUp` | The next page, the one before. A key that turns pages or reads waits for the page that was asked for before it: two typed at once are one. |
| `Down`, `Tab`, `Up` | From the line to find down to the rows, row by row, and back. |
| any character, `Backspace` | On the line to find: typed there, accented letters included. |
| `2`, `4`, `5` | Before a row: to change it, to remove it, to display it. `Backspace` takes it back. |
| `Enter` | On the line to find: reads the list from its first page, keeping the rows that hold what is typed there (empty, every row). On a row: does what was typed before it, or displays it. |
| `F6` | The form of a row to add. |
| `F5` | Reads the page again. |
| `F3` | Leaves the panel: its screen is no more. |
| `Esc` | Shows the overview; the panel stays open, as it is. |
| `Alt`+digit, `Alt`+`E` | Its screen, and the menu of screens, as for any screen. |

In the form:

| Key | What it does |
|---|---|
| any character, `Backspace`, `Delete`, `Left`, `Right`, `Home`, `End` | Typed in the field, 48 characters at most. |
| `Tab`, `Down`, `Up` | The next field that is typed, the one before. |
| `F4` | What the field may hold, when its line says so (`among`): `Up`, `Down`, then `Enter` picks, `F12` goes back. |
| `Enter` | Writes the row, and goes back to the list. |
| `F12` | Goes back to the list; nothing is written. |

- **What is written goes through the statements of SQL**, as if it were
  typed at the prompt of the screen: `insert`, `update ... where id =`,
  `delete ... where id =`. So the values by default, `not null`,
  `unique` and the checks of the table hold, who listens to the table
  hears of the row, and the programs `on insert` and `on update` run.
  The message line says what the statement said: `1 row added to items,
  with id 4.`
- **What the table refuses is said in red, in its words**, and the form
  stays as it was typed: `Column qty takes a whole number.`
- **A field of a number takes a calculation**: `6 * 2` is written as 12.
  A field of a text is written as it is typed.
- **A field where nothing is typed** is left to the table when a row is
  added (its value by default, or none), and is emptied when a row is
  changed.
- **`4` asks before it removes**: `Remove this row? [Enter] removes it,
  [F12] keeps it.`
- **`F4` proposes, the table decides.** What `among` lists is there to
  pick from; anything else may still be typed, and it is a `check` of
  the table that refuses a value.
- **A row that went while its form was typed** (removed by another
  prompt, a job) is said: `This row is no longer there.` Nothing is
  written.
- **A form that is being typed is not disturbed** by what others store:
  the list is read again when it is shown again.

- **A panel that is open is a virtual screen**: the first has `Alt`+`3`,
  after the overview, the log and the companion's. Four may be open at
  one time; a fifth is refused (`Four panels are open: leave one with
  [F3].`). A panel that is left gives its place back, and the others
  keep theirs. `screen` and the view `screens` list them.
- **The rows are in the order they were made**, by their `id`, with no
  limit to how many: a page is read when it is shown. There is no
  sorting yet.
- **The line to find** looks in the text columns of the list, the first
  three of them, for what is typed, anywhere in the text. `%` and `_`
  in it stand for any characters and any one character, as in `like`.
  A list with no text column says so. What few rows hold is looked for
  in the whole table at once, which a large table makes long.
- **Numbers are to the right** of their column, texts to the left. A
  column is as wide as what it holds on the page and as its label; when
  the row is wider than the screen the widest texts are cut, and no
  number is.
- **The list follows its table**: when rows are added, changed or
  removed by a statement of SQL, whoever typed it (another prompt, a
  job), the page that is shown is read again within a second. What the
  system writes for itself (the companion's `talk` and `acts`,
  `job_log`) is seen at `F5`.
- **What cannot be read is said in red** on the message line (a table
  that is gone, a column that was dropped), and stays said until the
  list can be read again.
- **`PgUp` remembers 64 pages back**; further than that it goes to the
  first page.

## Limits

| What | Limit |
|---|---|
| Panels open at one time | 4 |
| Columns of a list | 8 (a table has no more) |
| Rows of a page | What the screen has under the head of the panel: 37 on a screen of 50 rows |
| Fields of a form | 7 (a table has eight columns, `id` among them), of 48 characters |
| Values `F4` proposes | 40 |
| The line to find | 40 characters, in three text columns |
| A title, a label | 32 characters |

What is not there yet: panels opened from a row; programs on a key;
sorting; several rows acted on at one `Enter`; a field that is checked
before `Enter`; a panel on a view of the system or on two tables.

## How it is implemented

`core/src/panel.rs`. `Panel::read` reads the lines against the columns
of the table (`sql::Schema`) into a small value: the title, the columns
of the list, the fields. The lines are kept by the functions that keep
programs (`lang::store_in`, `load_from`), in another table. The panels
that are open are in `System::panels`, four places, each the virtual
screen `SCREENS.len() + place`; `ui.rs` gives the keys of such a screen
to `panel::key` and its rows to `panel::draw`, and `panel::screens`
tells the menu, `screen` and the view `screens` which screens there
are.

A page is read by `panel::read` through a `sql::Cursor` over `select
id, columns from table where id > N`, with `and (column like '%word%'
or ...)` when something is looked for: the cursor walks the table by
its `id` from `N`, so a page deep in a large table costs no more than
the first. One row more than the page shows is asked for, which tells
whether a page follows. The page is kept as text already laid out, with
the `id` of each row: 6 KB a panel.

The form keeps what is typed in each field (`Input`) and the `id` of
its row. `Enter` builds the statement and gives it to `shell::change`,
the function behind every `insert`, `update` and `delete` of the
prompt, as the session of the screen; what it says back is the message.
`F4` reads the line of the field again from the table `panels` and
takes what follows `among`.

A page is read at the tick of the interface, and nowhere else, and so
is everything a key asks of the tables: the row of a form, the
statement that writes it, the values of `F4` (`Todo`, done by
`panel::work`). A key or
the command that opens a panel only says that the page is to be read;
`panel::refresh`, called at every tick, reads it then, and reads it
again when `System::stored_at` says something was stored since, a
second apart at most. The cursor and the rows as values, 40 KB, are in
memory the interface keeps for that (`Ui::page`), not on a stack. It
was not so at first: read by the command that opened the panel, then by
the key that used the line to find, the page stood on a stack that was
deep already, and the system froze in QEMU both times.
