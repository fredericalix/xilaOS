# A first application: the stock

The stock is the first application of XilaOS made of
[panels](15-panels.md): equipment of all kinds, followed in two ways.
*Things* are followed one by one: an oscilloscope, with its serial
number, where it is and what state it is in. *Articles* are followed by
their quantity: cables, screws, with what came in and what went out.

Nothing of it is in the system. It is a text of statements,
`apps/stock.xs`, that makes four tables, four panels and three
programs. It is the worked example of what an application is here:
tables, and screens described on them.

## Putting it on a machine

On the machine, at its own keyboard:

```
XilaOS|all:/>remote on
```

which shows a code and the address to use. Then, from another machine
of the same network, in the repository:

```sh
python3 tools/remote.py 192.168.1.11 K7QD-2MXP --play apps/stock.xs
```

Every line of the file is typed at the prompt of the machine, in its
order, and what the machine answers is shown:

```
Table categories created.
Table items created.
Check added to items: every row holds it.
Table articles created.
Table movements created.
Index made on movements (article), from 0 rows.
4 rows added to categories, with ids 1 to 4.
Program on_insert_movements kept: 5 lines.
It runs on insert into movements. What it says goes to the log.
Program short kept: 2 lines.
Program recount kept: 8 lines.
Panel categories kept: 2 lines.
Panel moves kept: 5 lines.
Panel articles kept: 11 lines.
Panel items kept: 9 lines.
```

It stops at the first line that is refused, and says which. `remote
off`, at the machine, closes the door again. Nothing of this is
encrypted ([Networking](10-networking.md)).

- **Played twice, it stops at its first statement**, the table being
  there already (`Line 13 was refused`, `A table of that name
  exists.`): nothing is made twice, and nothing that was typed since
  is lost. A file that stopped half way is finished by hand, from the
  line it names, or begun again once what it made is dropped.
- **A file is looked at whole before its first line is sent.** A line
  of more than 240 characters, or with a letter outside ASCII (the
  tool does not carry accents yet), is named and nothing is sent.
- **No block is left open at the prompt**: a file whose last block has
  no `end` is refused when its end is reached (`The block opened at
  line 37 is not closed: nothing of it was kept.`).
- **By hand as well.** The file is about sixty lines to type, at any
  prompt, the screen's included: none is longer than its edit line.
- **Who can send it.** Whoever has the code of the prompt. The
  programs of its panels then run for who presses the key at the
  machine, with what the screen is allowed
  ([Panels](15-panels.md)): play there only a text that was read.

## Using it

At the screen of the machine:

| Type | For |
|---|---|
| `articles` | What is followed by its quantity. |
| `items` | What is followed one by one. |
| `categories` | The categories both may have. |

Each opens a panel on a screen of its own ([how a panel is
used](15-panels.md#using-one)).

**Articles.** `F6` adds one: its label, its category (`F4` proposes
those of `categories`), where it is, how many there should be at least.
Its quantity is shown and not typed: it starts at nothing and is the
sum of its movements.

```
 Articles                                                                page 1
 Find:

 Opt Label            Category  In stock  At least  Where it is
  _  Cat6 cable 5m    cables           4         5  shelf B
  8  Power cable C13  cables          12         5  shelf B

 2=Change  4=Remove  5=Display  8=Moves  F8=Short  F9=Recount
 F3=Exit  F5=Refresh  F6=Add  PgUp/PgDn=Pages  Enter=Go  Esc=Overview
```

- **`8` before an article, then `Enter`, opens its movements.** `F6`
  there adds one: a quantity, less than nothing for what goes out, and
  why. Its day is written by a program, and the quantity of the
  article follows within a second. `F3` goes back to the articles.
- **`F8` says how many articles are short**, under what they should
  have at least: `Articles under their least: 1`.
- **`F9` counts every quantity again** from the movements
  (`Quantities counted again from the movements.`): for the day a
  movement was changed or removed, or was missed (see the limits).

**Things.** `items` lists them; `F6` adds one, with its serial number,
where it is, since when, and its state: `good`, `worn`, `broken` or
`lent`, which `F4` proposes and the table checks. The line to find
looks in the label, the state and the place: `lent` there lists what
is lent.

## What it is made of

| Table | Columns |
|---|---|
| `categories` | `name` |
| `items` | `label`, `category`, `serial`, `place`, `state`, `since`, `note` |
| `articles` | `label`, `category`, `place`, `qty`, `least`, `note` |
| `movements` | `article`, `day`, `qty`, `why` |

- **The tables hold the rules.** A label is asked of every thing and
  every article, and no two articles have the same; the state of a
  thing is one of four, or none; the quantity of an article is 0
  until something comes in, and never nothing. A panel only shows
  what the table said when it refused a row.
- **A program that hears of movements keeps the quantities**:

  ```
  on insert into movements
      let which = id
      update articles set qty = qty + new.qty where id = new.article
      if new.day is null
          update movements set day = today where id = which
      end
  end
  ```

  (`let which = id` first: in the `update` that follows, `id` alone
  would be the column of the table, and every row would be dated.)
  It runs whoever adds the movement, the panel or a prompt, a moment
  after the row is stored: one movement at a time, each in its turn.
- **`recount` is the other way to the same number**: for every
  article, the sum of its movements, written as its quantity. What
  the first program keeps as rows come, this one works out from what
  is stored.
- **The panel of movements is opened from an article**
  (`option 8 'Moves' open moves where article = id`): it lists the
  movements of that article and gives its `id` to the ones that are
  added. Opened by its name, `moves` lists them all, and one added
  there has no article: the table refuses it.
- **Everything is SQL underneath.** `select label, qty from articles
  where qty < least` at a prompt answers what `F8` counts, and the
  companion reads the same tables.

## Its limits

They are those of the system today.

- Lists are in the order things were entered, not sorted; the line to
  find is how one is found.
- A category that is removed stays in the rows that had it: there are
  no keys from a table to another yet.
- A movement that is changed or removed does not change the quantity
  back: only adding one does. A wrong one is corrected by one more,
  or `F9` counts again.
- **Movements that come faster than the machine follows are stored
  but not counted, nor dated.** The system keeps sixteen changes
  waiting for the programs that hear of them, and each movement makes
  three (its row, its quantity, its day): typed in the panel they
  never crowd, but four statements of eight movements at a prompt
  left seven of thirty-two uncounted in a trial in QEMU, with a red
  line in the log (`N events came too fast for the programs that wait
  for them.`).
  The same if the power is cut between a movement and its program.
  `F9` puts the quantities right; the days stay empty. Importing
  rows in number is what [batch work](13-status-and-roadmap.md) is
  for, and waits for it.
- A movement typed at a prompt for an article that does not exist is
  kept and counted nowhere; an article that is removed leaves its
  movements behind. Both for the same reason as the categories.
- On a machine whose clock tells no time, movements are counted and
  not dated.
- One keyboard, one person: nothing says who made a movement.
- Whoever can type at a prompt can change these tables: there are no
  rights yet.
