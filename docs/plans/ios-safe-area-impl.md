# iOS safe area: implementation (commit 560011e)

Follows ios-safe-area-plan.md with REPORT #34 applied:

1. Floating menus (BLOCKING): web/src/safe-area.ts measures the --safe-* variables with a fixed
   probe (resize/orientationchange) into a reactive `insets`; `floating(extra)` returns
   `collisionPadding` = inset + 8 per side merged with extra props. Passed as `:content` to every
   UDropdownMenu, USelect, USelectMenu, UInputMenu and UTooltip (ChannelsPage keeps align end).
2. Drawer: treated as bottom (the default); content pads bottom/left/right; top/bottom drawers get
   max-h calc(96% - safe-top - safe-bottom) via an appended compound variant.
3/4. Modal: appended compound variant (scrollable false, fullscreen false) with top centred in the
   safe rectangle, calc(50% + (top - bottom)/2), and max-h for base and sm.
Dark band: body::before, fixed, z-index 2147483647, pointer-events none, height var(--safe-top).

Verified in Chrome, iPhone 16 Pro emulation + CDP Emulation.setSafeAreaInsetsOverride:
- portrait 62/34: all pages scrolled to the bottom, no hit-testable control outside 62..840;
  bottom-row dropdown flips above (556..774); sidebar drawer links end at 832; slideover header 62,
  close 78, last button ends 824; modal normal 300..602, forced 2000 px content 97..805 (centred at
  451); toaster viewport bottom 16 + margin 34.
- landscape 874x402 insets 0/21/62/62, dark: no control outside the safe rect.
- desktop 1280x800: no padding, band 0, unchanged.
pnpm build (vue-tsc) and 20 web tests pass. Screenshots in the scratchpad ios/ dir
(a local scratch dir).
Not verifiable here: native status glyph contrast, keyboard-open behaviour on a real iPhone.
