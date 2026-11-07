# Review: iOS home-screen app fixes (uncommitted, lineup, `git diff -- web`)

Owner report (real iPhone, home-screen app, reproduced in iOS 27.2 Simulator, iPhone 18 Pro):
1. Top: grayish blurred area covering the top of the app.
2. Bottom: a dead white strip that is not tappable.
3. Scrolling "breaks".

Measured in the Simulator with a temporary overlay (removed) while running black-translucent:
- screen 874 pt, innerHeight/clientHeight 812, but the page drawn from y=0 => `fixed inset-0` ended 62 pt
  above the screen bottom (+34 pt safe-bottom padding on the group = ~96 pt dead strip).
  The value is not stable: on another launch innerHeight was 874.
- A swipe scrolled the document (scrollY 39, fixed group top -39) instead of the panel.
- iOS 26+ draws a frosted blur under a translucent status bar, over the app header; our dark
  body::before strip turned into a gray gradient in light mode.

Changes:
- index.html: status-bar-style black-translucent -> default. Measured: safe-top 0, inner = client =
  812, page starts below the status bar, bar follows light/dark, no blur over content. Requires
  re-adding the icon (iOS reads the meta at add time).
- main.css: removed the dark body::before strip; html/body overflow hidden + overscroll-behavior none.
- vite.config.ts: group no longer pads bottom; dashboardPanel body pads bottom by safe-bottom, so
  lists scroll under the home indicator and their end clears it.
- safe-area.ts: on focusout, scroll the document back to 0,0 (measured scrollY 62 after focusing a
  field in the slideover even with overflow hidden).

Verified: Simulator light + dark, list scroll to last item, swipes on header/list, slideover
open/focus/blur; build, vue-tsc, vitest pass. Not verified: real software keyboard (the Simulator
keeps a hardware keyboard attached), landscape.

Please review for: regressions on desktop or iOS Safari (non-standalone), Android/Chrome PWA,
overlays (slideover/drawer/modal/toaster padding still using --safe-top which is now 0 in standalone
portrait), pages whose UDashboardPanel does not use the #body slot (bottom padding missing?), and
whether the focusout reset can fight the keyboard when focus moves field-to-field.
End with AGREE or BLOCKING.
