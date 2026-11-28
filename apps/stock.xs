# A stock of equipment for XilaOS: the first application made of panels.
#
# Things followed one by one (items), articles followed by their quantity
# (articles) with what comes in and goes out (movements), and the
# categories both may have. Nothing here is part of the system: these are
# statements, typed at a prompt or sent to one by
#
#     python3 tools/remote.py ADDRESS CODE --play apps/stock.xs
#
# Then, at the screen of the machine: articles, items, categories.
# See docs/16-the-stock.md.

create table categories (name text not null unique)
create table items (label text not null, category text, serial text, place text, state text default 'good', since text, note text)
alter table items add check (state in ('good', 'worn', 'broken', 'lent'))
create table articles (label text not null unique, category text, place text, qty int not null default 0, least int default 0, note text)
create table movements (article int not null, day text, qty int not null, why text)
create index on movements (article)

insert into categories values ('cables'), ('tools'), ('measure'), ('parts')

# What comes in or goes out of an article changes its quantity, and is
# dated with the day it was typed.
on insert into movements
    let which = id
    update articles set qty = qty + new.qty where id = new.article
    if new.day is null
        update movements set day = today where id = which
    end
end

# How many articles have less than they should.
program short
    let under = select count(*) from articles where qty < least
    say 'Articles under their least: ' + under
end

# The quantities, counted again from the movements: for the day one was
# changed or removed, or came while the machine could not follow.
program recount
    for one in select id from articles
        let total = select sum(qty) from movements where article = one.id
        if total is null
            let total = 0
        end
        update articles set qty = total where id = one.id
    end
    say 'Quantities counted again from the movements.'
end

panel categories on categories
    title 'Categories'
    field name 'Name'
end

# Opened from an article: its movements, and a new one is one of its own.
panel moves on movements
    title 'Movements'
    list day, qty, why
    field day 'Day' shown
    field qty 'Quantity (- out)'
    field why 'Why'
end

panel articles on articles
    title 'Articles'
    list label, category, qty, least, place
    field label 'Label'
    field category 'Category' among select name from categories
    field place 'Where it is'
    field qty 'In stock' shown
    field least 'At least'
    field note 'Note'
    option 8 'Moves' open moves where article = id
    key f8 'Short' run short
    key f9 'Recount' run recount
end

panel items on items
    title 'Things'
    list label, state, place, serial, category
    field label 'Label'
    field category 'Category' among select name from categories
    field serial 'Serial number'
    field place 'Where it is'
    field state 'State' among 'good', 'worn', 'broken', 'lent'
    field since 'Since'
    field note 'Note'
end
