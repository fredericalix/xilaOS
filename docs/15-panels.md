# Panels: the screens of applications

A *panel* is the screen of an application: a list of the rows of a
table, under labels that are chosen, on a virtual screen of its own. It
is described in a few lines, on a table, and the system gives the rest:
the pages, the line to find, the keys. It is what a display file and a
"work with" screen are on an AS/400, with less to write.

It was built on 28 November 2026, in three steps: the list, the form,
then the options and keys of a panel's own
([Status and roadmap](13-status-and-roadmap.md#application-screens-panels)).

## At a glance

```
XilaOS|all:/>create table items (label text, place text, qty int)
XilaOS|all:/>panel stock on items
..>title 'The stock'
..>list label, place, qty
..>field label 'Label'
..>field place 'Where it is'
..>field qty 'In stock'
..>end
Panel stock kept: 5 lines.
XilaOS|all:/>stock
```

The last line opens it:

```
 The stock                                                              page 1
 Find: _

 Opt Label           Where it is  In stock
  _  Oscilloscope    bench 2             1
  _  Soldering iron  bench 1             2
  _  Cat6 cable 5m   shelf B            12


 2=Change  4=Remove  5=Display
 F3=Exit  F5=Refresh  F6=Add  PgUp/PgDn=Pages  Enter=Go  Esc=Overview
```

`F6` there, or `2` before a row, shows the form:

```
 The stock                                                      row 3, changed

 Label .......... : Cat6 cable 5m
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
| `option 8 'Label' open PANEL where COLUMN = id` | A digit typed before a row opens another panel on the rows that belong to it: those whose `COLUMN` holds the `id` of this row. |
| `option 8 'Label' run PROGRAM(values)` | The same digit runs a program instead. |
| `key f8 'Label' run PROGRAM(values)` | A function key runs a program. |
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
  panel: a panel runs programs at a key, for who sits at the machine.
- **With no `field` line, every column but `id` is a field**, under its
  name. With some, the form has those and no other: a column without a
  field is not shown there, and takes its value by default when a row
  is added.
- **Options and keys of its own**, eight in all: the digits 1, 3, 6, 7,
  8 and 9 (2, 4 and 5 are the system's), the keys F1, F2 and F7 to F11.
  Their labels, 12 characters at most, are shown with the system's on
  the row before last, and all of them must fit there: a panel whose
  options would not all be shown is not kept, so that no key does
  something the screen does not say. A program it runs is named as a
  program is: not as a command.
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
| `F3` | Leaves the panel, and what was typed with it, without asking. |
| `Esc` | Shows the overview; the form stays as it is typed. |

- **What is written goes through the statements of SQL**, as if it were
  typed at the prompt of the screen: `insert`, `update ... where id =`,
  `delete ... where id =`. So the values by default, `not null`,
  `unique` and the checks of the table hold, who listens to the table
  hears of the row, and the programs `on insert` and `on update` run.
  The message line says what the statement said: `1 row added to items,
  with id 4.`
- **What the table refuses is said in red, in its words**, and the form
  stays as it was typed: `Column qty takes a whole number.`
- **A field of a number takes a number, or a calculation on numbers**:
  `6 * 2` is written as 12. It is worked out by the form, which knows no
  column and no name there: anything else is refused before anything
  is written (`In stock takes a number, or a calculation.`). A field of
  a text is written as it is typed, quotes and all. Whatever is typed
  in a field is one value of its column, and nothing more.
- **A change writes the fields that were changed, and no other.** A
  field that was not touched is not written back: not a text longer
  than the 48 characters the form shows of it (the lines of programs
  are), not an empty text, not what another prompt or a job wrote there
  while the form was open. With nothing changed, `Enter` says `Nothing
  was changed.`
- **A field where nothing is typed** is left to the table when a row is
  added (its value by default, or none), and is emptied when it held
  something and a row is changed.
- **A field that has some `field` lines has only those in its form**: a
  column is given a label for the list by a `field` line, and that line
  also puts it in the form. A column left out is not typed, and takes
  its value by default in a row that is added.
- **`4` asks before it removes**, naming the row by its `id`: `Remove
  row 12? [Enter] removes it, [F12] keeps it.` What is typed before a
  row stays with that row when the list is read again, wherever the
  row is then, and is dropped if the row is gone.
- **`F4` proposes, the table decides.** What `among` lists is there to
  pick from; anything else may still be typed, and it is a `check` of
  the table that refuses a value.
- **A row that went while its form was typed** (removed by another
  prompt, a job) is said: `This row is no longer there.` Nothing is
  written.
- **A form that is being typed is not disturbed** by what others store:
  the list is read again when it is shown again.

**A panel opened from a row.** `8` before an article, then `Enter`,
opens the panel of its movements, on its own screen, with what it is
opened on beside its title:

```
 Moves  (article = 3)                                                   page 1
```

- **Only the rows of that row are listed**, and the line to find
  narrows them further.
- **A row that is added there belongs to that row**: its column takes
  that value, which the form shows and does not let type.
- **The column is one of whole numbers**, since it holds the `id` of a
  row: anything else is refused on the panel that asked.
- **`F3` goes back to the panel it was opened from**, if that one is
  still open.
- **Opened again from another row**, the panel that is open under that
  name is opened on those rows: there is one of each name. Not while a
  form is typed there, or a program of it runs: `moves is open on a
  form: finish it there, or leave it, first.`
- What cannot be opened says why on the panel that asked: a panel that
  is not kept, a table with no such column, four panels open.

**A program run from a panel.** `key f8 'Count' run recount(id, qty)`
runs the program when `F8` is pressed; `option 9 'Zero' run zero(id)`
when `9` is typed before a row and `Enter` pressed.

- **The values are worked out with the columns of the row as names**:
  `id`, and each column of the table. A key pressed with the keys on no
  row has no such names: `This key is for a row: go down to one first.`
  A program that takes no value of a row runs from anywhere.
- **It runs as a job nobody waits for**, started by the screen: the
  message says `Job 7 runs: its last line will be said here.`, the keys
  stay free, and when it has ended its last line is the message, and
  the list is read again. If it failed, the message is red and says
  why and where: `Division by zero. Stopped at line 1 of recount.`
- **One at a time**: while it runs, a key or an option of the panel
  that runs a program answers `Job 7 still runs: wait for it, or type
  job 7 end at the prompt.` A key that is held is not a program run
  several times. `jobs` and `job 7` show it,
  and it is written in `job_log`, as any such job.
- **A program gives nothing back**: what it has to say to the panel is
  its last `say`.
- **It runs for who sits at the machine.** A panel, as a program, may
  have been written from the prompt over the network, where what takes
  a disk or opens the machine is refused; run from a key at the
  machine, its program is refused nothing of that. Read a panel
  (`show panel`) and the programs it names before pressing its keys,
  as one reads a program before running it.

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
| Options and keys of a panel's own | 8, with labels of 12 characters |
| Values `F4` proposes | 40; 32 when its `select` sorts them, and it says so when there are more |
| A label in the form | Its first 26 characters |
| The line to find | 40 characters, in three text columns |
| A title, a label | 32 characters |

What is not there yet: sorting; several rows acted on at one `Enter`; a
field that is checked before `Enter`; a program that fills a field or
decides what a form shows; a panel on a view of the system or on two
tables.

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
the `id` of each row; with its form, a panel that is open is 8 KB.

The form keeps what is typed in each field (`Input`) and the `id` of
its row. `Enter` builds the statement and gives it to `shell::change`,
the function behind every `insert`, `update` and `delete` of the
prompt, as the session of the screen; what it says back is the message.
`F4` reads the line of the field again from the table `panels` and
takes what follows `among`.

An option or a key of the panel's own keeps its trigger, its label and
its line (`Action`); what it does is read again from that line when it
is used (`does`). Opening a panel from a row gives the new one the
column and the value it is opened on (`Open::only`), which its list
adds to its `where` and its form to its `insert`, and the place it came
from (`Open::from`). Running a program reads the row, makes its columns
the names of the screen's prompt for the time of the call, and starts
the program as a job (`jobs::begin`, nobody waiting), whose number the
panel keeps until the job has ended.

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
