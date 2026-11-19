# The Trame mock-up

`trame-0.3/` is the interactive web mock-up that XilaOS's interface was
drawn from: the header, the title bar, the terminal under a city, the
colors of the day and night themes. Open `trame-0.3/index.html` in a
browser; everything is local.

It is kept as it was received, so it is in French (its notes are in
`LISEZMOI.txt`), and it goes further than the system does: a
spreadsheet, card stacks, a game, music, agents. None of that exists
in XilaOS.

Where XilaOS differs on purpose:

- the font is the 8x8 font of V2_OS, not the mock-up's VT323 and
  Silkscreen;
- everything the system shows is in English;
- the keyboard is the only input: the mock-up also takes the mouse;
- the city is a fixed isometric drawing made by the processor, not a
  3D scene with a camera;
- the sample data of the mock-up describes a Unix machine; XilaOS is
  not one.

The mock-up bundles three.js (MIT) and the Silkscreen and VT323 fonts
(SIL Open Font License). Their licences are in `trame-0.3/licences/`.
