# The companion

XilaOS has a companion: something to ask, at the prompt, which answers
in a few lines and reads the tables to do so. Its name is Xila.

It does not think here. A question is sent, over HTTPS, to a language
model on the Internet, with what was said before; what the model says
back is the answer. What makes it a companion of *this* system, and
not a window on a chat service, is what it is told and what it may do:
it is told what XilaOS is, and it has a way to look at the machine it
lives in.

> **What is asked leaves the machine.** A question, the last turns of
> the conversation and the rows of every table the companion looked at
> to answer are sent to the model's server. They travel sealed (TLS),
> and the server reads them: that is what it is for. Do not ask it
> about tables that must stay here.

This is its first form. It answers and it reads. It does not act, it
keeps no notes, it does not react to what happens, and it has no
screen of its own yet: see [what is left](#what-is-left).

## Asking

It needs three things: a network that leads to the Internet, a key to
the model's server, and a processor with a source of chance of its own
(the boot log says so on its `Chance` line), without which XilaOS
keeps nothing secret and so asks nothing through TLS.

The key is kept once, and never shown again (see
[Keys to services](03-using-xilaos.md#keys-to-services)):

```
XilaOS|all:/>key set openrouter ********
The key openrouter is kept. It is never shown.
```

Then `ask`, and whatever one wants to know, to the end of the line:

```
XilaOS|all:/>ask how many tasks are running, and what are they?
Three tasks: interface (running, 8%), idle (ready, 92%), and serial shell
(waiting, 0%).
XilaOS|all:/>ask which of my notes has the most stars?
"holidays" has the most stars, 5 (rent 3, taxes 1).
XilaOS|all:/>ask what is your name and where do you live?
I am Xila, and I live inside XilaOS, this little system you are typing at: in
its database, among its tables, one companion for its one user. No files, no
streets; my place is the prompt.
```

Those are real answers, from QEMU, each one to four seconds after the
question. On the screen the prompt is free at once: the answer is
written when it comes, under whatever was typed meanwhile, and the
machine goes on living while the model thinks. On the serial port the
prompt waits for the answer, which is what a program that drives the
machine wants. One question at a time: a second one, while the first
is being answered, is refused.

A question is 150 characters at most. An answer is plain text, cut
into lines at spaces; what the model writes outside plain ASCII
becomes `?`, or its plain form for the quotes and dashes of
typographers.

When there is no answer, one line says why: `No key opens the model.
Type key set openrouter and the key.`, `The model's server refused
(401): No auth credentials found.` (in the server's own words), `The
model's server does not answer.`, `What the model's server said is not
an answer.`, `The companion looked again and again without answering:
given up.`, or what `net get https://` says of a server it does not
trust.

## What it sees

The model has one tool, `select`: it asks for a query, the query is
run here as it would be at the prompt, and its rows are sent back. It
usually starts with `select * from tables`, then looks at the columns
of the table the question is about, then reads it. One question is so
several calls to the model, six at most, and at most four queries in
each.

It reads what SQL reads, and nothing else:

- it cannot change anything: its tool goes through the same door as
  the pages for browsers, which only takes `select`;
- it cannot see a key: SQL does not know the table keys are kept in;
- a result is what a result is at the prompt: 32 rows, texts of 48
  characters, and 900 characters of it in all are sent.

## What is said is kept

Everything goes into a stored table, `talk`, one row for each thing
said, under the number of the question it belongs to:

```
XilaOS|all:/>select turn, who, text from talk order by id
   turn  who     text
   1     you     how many tasks are running, and what are they?
   1     select  select count(*) from tasks
   1     select  select * from tasks
   1     result  count 3
   1     result  id name state ran_ms share 0 interface running 2
   1     xila    Three tasks: interface (running, 8%), idle (read
   2     you     which of my notes has the most stars?
   2     select  select * from tables
   2     result  name kind columns rows about tables system 5 12
   2     select  select * from columns where table_name = 'notes'
   2     select  select * from notes
   2     result  table_name position name type notes 1 id int not
   2     result  id label stars 1 rent 3 2 holidays 5 3 taxes 1
   2     xila    "holidays" has the most stars, 5 (rent 3, taxes
```

`you` is what was asked, `xila` what the companion said (`more` for
what did not fit in one row), `select` a query it asked for and
`result` what came of it. The texts are longer than SQL shows: a row
holds up to 900 characters, and `select` gives the first 48, with a
space for each line break, as it does for the lines of `programs`.

The last six turns go along with each new question, which is how the
companion knows what "and the other one?" is about. The table is the
user's: `delete from talk` makes it forget.

## Who it is

A row of another stored table, `companions`, made at the first
question:

```
XilaOS|all:/>companion
   item                value
   name                Xila
   model               deepseek/deepseek-v4.1-flash-1017
   at                  https://openrouter.ai/api/v1/chat/completions
   key                 openrouter: kept
   now                 nothing asked
   calls to the model  6
   queries run for it  5
```

| Column | What it is |
|---|---|
| `name` | What it is called, and told it is called. |
| `place` | Where its model is asked: any server that speaks the chat completions form of OpenAI. |
| `model` | Which model, as that server names it. |
| `key` | The name of the key that opens it (`key set`), not the key. |
| `character` | A few words about how it is to be, added to what it is told. |

All of it changes with `update`: `update companions set model =
'...'`, `update companions set character = 'You are wary and brief.'`.
The table can also be made by hand before the first question, with
those five text columns. Only its first row is read: the others are
for the companions to come.

The key goes out only where the way is sealed. A place that starts
with `http://` gets no key, and needs none to be asked: that is how
the checks speak to a made-up model on the development machine.

## How it is built

`core/src/companion.rs` has no network in it. `ask` takes a question,
writes it in `talk` and sets the companion to `Calling`; `request`
writes what is sent to the model, JSON, into a buffer; `heard` takes
what came back, keeps what the model said, runs the queries it asked
for (`sql::select` on a `&System`: a reference that allows no change)
and keeps their results, and either leaves the companion `Calling`,
for one more round, or ends the question with an `Outcome`.

The kernel carries the bytes, a little at each tick of the interface
task (`companion` in `kernel/src/main.rs`), as it does for the pages
for browsers: it finds the address of the server by its name, starts a
TLS client and an `Ask`, steps them until the answer is whole, hands
it to `heard`, and starts again if the companion is still `Calling`.
Nothing waits in a loop, so the keyboard, the screen and the serial
port go on. The session that asked is told when the outcome is there
(`companion::tell`): the screen at its next tick, the serial task by
looking at each tick before it writes its status line.

`core/src/json.rs` writes and reads JSON without a heap: writing goes
straight into the buffer, and reading finds a value under a name or at
a place in a list in the bytes as they are, with no tree. The buffers
for a question and its answer, 48 KB and 64 KB, are made by the kernel
on its heap at the first question.

The conversation sent to the model is rebuilt from the rows of `talk`
at each call, since the rows are all there is: a row `xila` followed
by rows `select` is one message of the model with its queries inside,
and each `result` is the message that answers one of them, under a
number made from the number of its row.

What the model is told before anything else is in `companion.rs`
(`TOLD`): that it lives in XilaOS, that XilaOS is not a Unix, what the
tables are and how small the SQL is, and to answer briefly in plain
ASCII.

It is checked against a made-up model on the host, which answers what
the checks tell it to (a query to run, an answer, a refusal, a pause),
and against the real one when a key and the Internet are there.

## What is left

- **A screen of its own**, where the conversation scrolls and what is
  typed is for the companion.
- **Acting**: running commands and changing data, with every action
  written in a table, a command that stops it, and `install`,
  `disk erase`, `remote on` and `web on` refused to it. Until then it
  only reads.
- **Notes** of its own, kept from one conversation to the next.
- **Reacting** to changes in a table, to the state of the system, to
  the clock; being called by a XilaScript program, and asked from the
  prompt over the network.
- Longer questions and answers, results that are not cut, and a
  conversation that is summed up instead of losing its oldest turns.
- The other companions: more rows of `companions`.

What it costs is the model's price for each call; nothing here caps
it. A text of a table can try to steer a model that reads it. As long
as the companion only reads, the worst such a text can do is make it
read more and say something wrong; that changes the day it can act.
