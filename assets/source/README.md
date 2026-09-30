# Source artwork

Raw images generated with ChatGPT's image model, all in one conversation so that the character
stays consistent. `cargo run -p asset-tool -- all` turns them into the sprites, icons and
installer images the project ships (see `tools/asset-tool`). The images contain no text.

The characters and motifs are original. They are inspired by the fan tradition of
personifying NetEase Cloud Music ("网易云娘": red, white and black, a vinyl record, clouds) but
deliberately **do not reproduce the official logo or any brand mark**, and the project is not
affiliated with NetEase.

## Design language: 黑胶红 / Vinyl Red

Modern clean anime illustration, soft cel shading, crisp dark outlines. Palette: NetEase-style
crimson (`#E60026` / `#C20C0C`), white and jet black, soft warm pink `#FFD9DE` for blush and
highlights, small gold accents for music-note charms and record grooves. In the UI the accent is
`#D9202F` with a warm amber secondary (`#FFB36B`).

## Mascot: Yin-chan (音音)

About 16 years old. Long twin-tails that are jet black at the roots and fade to vivid crimson at
the tips; large ruby-red eyes with star highlights; white over-ear headphones whose ear cups are
black vinyl records with red labels, a small white cloud-shaped hair clip and a tiny golden
music-note charm; a white sailor blouse with a crimson collar and a big crimson ribbon; a short
black pleated skirt with red piping; white knee socks with red ribbons; black loafers.

## Files

| File | Use | Notes |
| --- | --- | --- |
| `mascot_idle.png` | Sidebar (idle), reference sheet | Waving, three-quarter view |
| `mascot_working.png` | Sidebar while downloading | Sitting on a cloud holding a red download arrow, records and notes around her |
| `mascot_happy.png` | After a successful batch, About page | Arms up, petals and notes |
| `mascot_sad.png` | After a failed batch | Apologetic, sweat drop |
| `mascot_login.png` | Sidebar when logged out | Holding up a phone, winking |
| `mascot_search.png` | Empty states | Magnifying glass |
| `logo.png` | Application icon (.ico / .icns / PNG set) | Head-and-shoulders on a red gradient with a record halo |
| `banner.png` | Page header, README | Crimson dusk sky, a giant vinyl "moon", room for a title on the left |
| `installer_side.png` | Windows installer welcome image | Portrait |

Sprites are generated on a plain white background and cut out by `asset-tool`; in the app they
stand on a pale "stage" card, which also hides the small white gaps the cut-out keeps between hair
strands.

## Prompts

The first request describes the character in full (see above) and asks for a full-body,
three-quarter view on a plain white background with no text or logo. Later requests start with
"Same character as before, identical design" and change only the pose and props. The logo asks for
a square close-up portrait on a bright-red-to-crimson gradient with a vinyl-record halo; the banner
and installer images ask for the crimson dusk scene described in the table above. Every prompt
states that no text, watermark or logo may appear.
