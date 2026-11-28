# Panels: the screens of applications

A *panel* is the screen of an application: a list of the rows of a
table, under labels that are chosen, on a virtual screen of its own. It
is described in a few lines, on a table, and the system gives the rest:
the pages, the line to find, the keys. It is what a display file and a
"work with" screen are on an AS/400, with less to write.

What is built is the list (28 November 2026). The form to add and
change a row, panels opened from a row and programs on a key are the
next steps of the plan in
[Status and roadmap](13-status-and-roadmap.md#application-screens-panels).

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

 label           Where it is  In stock
 Oscilloscope    bench 2             1
 Soldering iron  bench 1             2
 Cat6 cable 5m   shelf B            12


 F3=Exit  F5=Refresh  PgUp/PgDn=Pages  Enter=Find  Esc=Overview
```

## Describing a panel

`panel NAME on TABLE` opens a block, as `program` does, and `end`
closes it. The table is a stored one, and must be there.

| Line | What it says |
|---|---|
| `title 'text'` | The title of the screen, 32 characters at most. Without it, the name of the panel. |
| `list a, b, c` | The columns of the list, in their order: eight at most, `id` among them if wanted. Without it, every column but `id`. |
| `field column 'Label'` | What a column is called on this panel: the list shows the label over it. One field a column, never `id`. |
| `field column 'Label' shown` | A column that is shown and not typed, for the form that is to come. |
| `field column 'Label' among ...` | What the column may hold, for the form that is to come: kept as it is written. |
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
  as a command.
- **The companion may not change the table `panels`**, nor drop a
  panel: a later step lets a panel run programs at a key.
- **A panel is read again each time it opens**, on its table as it is
  then. If a listed column is gone, or the table, it says so and does
  not open.

## Using one

Its name, typed alone at the prompt of the screen, opens it and shows
it. A panel opens at the screen of the machine only: at the serial
port and at the prompt over the network its name answers `A panel
opens at the screen of the machine: stock.`

| Key | What it does |
|---|---|
| `PgDn`, `PgUp` | The next page, the one before. |
| any character, `Backspace` | Typed in the line to find. |
| `Enter` | Reads the list from its first page, keeping the rows that hold what the line to find says. Empty, it shows every row. |
| `F5` | Reads the page again. |
| `F3` | Leaves the panel: its screen is no more. |
| `Esc` | Shows the overview; the panel stays open. |
| `Alt`+digit, `Alt`+`E` | Its screen, and the menu of screens, as for any screen. |

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
- **Numbers are to the right** of their column, texts to the left. A
  column is as wide as what it holds on the page and as its label; when
  the row is wider than the screen the widest columns are cut.
- **The list follows its table**: when rows are added, changed or
  removed by anyone (another prompt, a job), the page that is shown is
  read again within a second.
- **`PgUp` remembers 64 pages back**; further than that it goes to the
  first page.

## Limits

| What | Limit |
|---|---|
| Panels open at one time | 4 |
| Columns of a list | 8 (a table has no more) |
| Rows of a page | What the screen has under the head of the panel: 38 on a screen of 50 rows |
| The line to find | 40 characters, in three text columns |
| A title, a label | 32 characters |

What is not there yet: a form to add, change and remove rows; values a
field may take, shown at a key; panels opened from a row; programs on a
key; sorting; a panel on a view of the system or on two tables.

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
the `id` of each row: 4 KB a panel.

A page is read at the tick of the interface, and nowhere else. A key or
the command that opens a panel only says that the page is to be read;
`panel::refresh`, called at every tick, reads it then, and reads it
again when `System::stored_at` says something was stored since, a
second apart at most. The cursor and the rows as values, 40 KB, are in
memory the interface keeps for that (`Ui::page`), not on a stack. It
was not so at first: read by the command that opened the panel, then by
the key that used the line to find, the page stood on a stack that was
deep already, and the system froze in QEMU both times.
