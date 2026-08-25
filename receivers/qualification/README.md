# Qualification records

`../QUALIFICATION.md` is the gate; this directory is where a run of it
becomes a fact. One file per run, named `YYYY-MM-DD-<platform>-<device>.md`
(`2026-09-02-webos-oled55c3.md`), copied from `TEMPLATE.md` and committed —
the release rule is that every applicable row has an attached result, and an
uncommitted result is not attached.

Three rules, so the records stay evidence rather than optimism:

- **A row is `pass`, `fail`, or `n/a` — never blank.** An unexercised row is
  the release blocker it looks like. `n/a` carries the reason (a webOS run
  has no Siri Remote row).
- **Security and integrity rows are zero-tolerance.** A `fail` there is not
  a note for later; the run stops and the defect gets an issue before the
  next run starts.
- **The environment block is not optional.** A pass on unknown firmware
  against an unknown build qualifies nothing — the whole point of the record
  is that somebody later can say *this* build passed on *that* television.

The rows come from `../QUALIFICATION.md` verbatim. When that file gains a
row, the template gains it in the same change — the pre-existing records
stay as they were, because a record is what was true when it was taken.
