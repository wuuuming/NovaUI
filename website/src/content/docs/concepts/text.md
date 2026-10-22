---
title: Text
description: Fonts, the type ramp, shaping, bidirectional text and line breaking.
sidebar:
  order: 2
---

A `Text` element is one run of text in one style: its string, size, line height, weight,
alignment, a line limit and a colour.

- **Type ramp.** `type: body` takes size, line height and weight from a theme token; the WinUI
  ramp has `caption`, `body`, `body-strong`, `body-large`, `subtitle`, `title`, `title-large`
  and `display`. `font-size`, `line` and `weight` set them one by one.
- **Fonts.** The bundled Noto variable fonts first (Noto Sans, then SC, JP and KR, then Arabic,
  Hebrew, Devanagari and Thai), then the system's. Fallback is chosen per grapheme cluster, so
  text mixing scripts needs no markup. An app can put an installed family first
  (`Tree::set_font`).
- **Shaping.** Text is shaped with HarfBuzz's rules (`harfrust`): kerning, ligatures, Arabic
  joining, Indic reordering and conjuncts, marks placed on their letters. Right-to-left text
  (Arabic, Hebrew) is laid out by the Unicode bidirectional algorithm, mixed with left-to-right
  text in one line.
- **Weights.** A variable family is kept at every 50 from 400 to 700, so an animated weight gets
  a face of its own.
- **Lines.** Breaking is greedy at the Unicode line-breaking algorithm's opportunities: after
  spaces, between CJK characters, after hyphens, with closing punctuation kept with what it
  closes. `lines: 0` wraps without a limit, `lines: n` stops after n lines
  with an ellipsis. `text-align` is left, centre or right.
- **Glyphs** are rasterized at their physical size into a paged atlas; a full atlas evicts the
  page used longest ago and paints again only the texts that showed it.

## Per-unit text

A text can be drawn per word or per character (`units: word | char`), each unit with its own
time span (`timing`) and its own motion, colour and glow as expressions of time. This is how
karaoke lyrics are drawn; it is paint only and never moves layout.

## Limits

Thai, Lao and Khmer break only at spaces (no dictionary). Text is horizontal only, and not
justified. Alignment is the element's: a right-to-left paragraph in a left-aligned element
starts at the left. Colour emoji are not drawn.

## See also

[Text](/components/text/), [Edit](/components/edit/), [Theming](/theming/).
