---
name: BoxDB
description: A practical field manual for a typed, single-server database.
colors:
  cyan: "#057f9e"
  cyan-hover: "#076a84"
  pale-cyan: "#dff3f6"
  ink: "#10232d"
  muted-ink: "#4d6473"
  paper: "#f4f8f8"
  white: "#ffffff"
  rule: "#d7e2e7"
  docs-light-accent-high: "#09536a"
  docs-dark-accent: "#21aac7"
  docs-dark-accent-low: "#113f4d"
  docs-dark-accent-high: "#a9e9f4"
  docs-dark-paper: "hsl(224, 10%, 10%)"
  docs-dark-surface: "hsl(224, 14%, 16%)"
  docs-dark-rule: "hsl(224, 10%, 23%)"
  docs-dark-body: "hsl(224, 6%, 77%)"
  docs-dark-heading: "hsl(224, 20%, 94%)"
typography:
  display:
    fontFamily: '"Lexend Variable", sans-serif'
    fontSize: "clamp(3.2rem, 6.3vw, 6rem)"
    fontWeight: 700
    lineHeight: 1.04
    letterSpacing: "-0.04em"
  headline:
    fontFamily: '"Manrope Variable", sans-serif'
    fontSize: "clamp(2rem, 3.4vw, 3.25rem)"
    fontWeight: 700
    lineHeight: 1.12
    letterSpacing: "-0.04em"
  title:
    fontFamily: '"Manrope Variable", sans-serif'
    fontSize: "23px"
    fontWeight: 700
    letterSpacing: "-0.03em"
  body:
    fontFamily: '"Manrope Variable", sans-serif'
    fontSize: "17px"
    fontWeight: 400
    lineHeight: 1.65
  docs-body:
    fontFamily: '"Manrope Variable", sans-serif'
    fontSize: "1rem"
    fontWeight: 400
    lineHeight: 1.8
  label:
    fontFamily: '"Manrope Variable", sans-serif'
    fontSize: "13px"
    fontWeight: 400
  specimen-code:
    fontFamily: '"JetBrains Mono Variable", monospace'
    fontSize: "clamp(0.9rem, 1.38vw, 1.25rem)"
    fontWeight: 300
    lineHeight: 1.7
rounded:
  compact: "4px"
  control: "8px"
  code-panel: "12px"
spacing:
  xs: "8px"
  sm: "12px"
  md: "16px"
  lg: "20px"
  xl: "24px"
  section-gap: "32px"
  column-gap: "36px"
  section-inset: "48px"
components:
  button-primary:
    backgroundColor: "{colors.cyan}"
    textColor: "{colors.white}"
    rounded: "{rounded.control}"
    padding: "16px 26px"
  button-primary-hover:
    backgroundColor: "{colors.cyan-hover}"
  button-secondary:
    textColor: "{colors.ink}"
    rounded: "{rounded.control}"
    padding: "16px 26px"
  button-secondary-hover:
    backgroundColor: "{colors.pale-cyan}"
  client-option:
    textColor: "{colors.ink}"
    rounded: "{rounded.control}"
    padding: "22px 12px"
  client-option-selected:
    backgroundColor: "{colors.pale-cyan}"
  code-panel:
    backgroundColor: "{colors.white}"
    rounded: "{rounded.code-panel}"
---

# Design System: BoxDB

## Overview

**Creative North Star: "Technical Field Manual"**

BoxDB uses cool paper, deep ink and cyan to make a developer tool feel direct and inspectable. Working SQL and readable code carry the visual interest; thin rules and generous section spacing keep the surrounding explanation quiet. Preserve the existing cyan B icon and the plain English developer voice.

The landing page is an invitation to try a client; documentation is a denser reading environment with the same Manrope voice and cyan identity. The built landing display uses Lexend, while Manrope handles supporting headings, body text and documentation. Keep experimental status visible and factual; visual confidence must not imply adoption, performance or production readiness that the project has not established.

**Key Characteristics:**

- Cool neutral surfaces with cyan reserved for identity, selected states and meaningful emphasis.
- Large, compact landing display paired with calm, spacious technical prose.
- Real code, ruled ledgers and SVG diagrams instead of decorative imagery.
- Native links and controls with visible keyboard focus and narrow-screen reflow.

## Colors

The palette combines blue-tinted paper and ink with one cyan accent family. The frontmatter records the reusable colors; source CSS remains the implementation authority.

### Primary

- **Cyan:** solid landing SQL field, ordinary primary actions, light documentation links and current navigation.
- **Cyan Hover:** the darker ordinary primary-action hover state.
- **Pale Cyan:** installation strip, selected client, icon discs and closing band.
- **Documentation accents:** light mode uses the shared cyan/pale pair and a deep cyan high tone; dark mode uses separate low, accent and high tones to preserve contrast.

### Neutral

- **Ink / Muted Ink:** headings and supporting landing text; do not replace supporting text with low-contrast gray.
- **Paper / White / Rule:** landing canvas, code surfaces and quiet dividers. Light documentation uses white content against cool navigation surfaces.
- **Dark documentation neutrals:** near-black content, slightly lighter navigation and code surfaces, subdued rules, pale body text and brighter headings. These are Starlight's inherited dark neutrals, not an inverted landing palette.

**The Cyan Field Rule.** Cyan may occupy a whole functional code field as well as small controls; use it to identify something worth inspecting or acting on, not as unrelated decoration.

## Typography

**Display Font:** Lexend Variable, sans-serif. **Body Font:** Manrope Variable, sans-serif. **Specimen Font:** JetBrains Mono Variable, monospace. These fonts are self-hosted. Documentation code and the compact terminal/result treatments retain their existing system-monospace stacks.

The display is heavy and tightly tracked; supporting type is less compressed and gives technical content room to breathe. Font sizes are role-specific rather than a single mathematical scale.

### Hierarchy

- **Display:** the frontmatter display role is for the landing headline. Below the landing mobile breakpoint it becomes `clamp(2.9rem, 9.2vw, 4.5rem)`.
- **Headline:** major landing sections use the frontmatter headline role, becoming (34px) on mobile. Benefit titles use the smaller title role.
- **Body:** landing explanatory text uses the body role, generally constrained to (38–47ch); the hero introduction has its own fluid size and (42ch) limit.
- **Documentation:** body uses the docs-body role; headings are Manrope (600), line-height (1.2), tracking (-0.025em). Starlight changes h1/h2 from (35px/29px) to (42px/35px) at its desktop breakpoint. Markdown h2/h3 receive (2em) top spacing.
- **Label and code:** compact labels stay secondary; code remains selectable text. The cyan specimen uses the specimen-code role and becomes (13px) on mobile. Landing examples use (14px/1.8), becoming (12px) on mobile.

**The Code Is Content Rule.** Keep code as text with intentional line breaks and local overflow; do not rasterize it to preserve a composition.

## Layout

Landing content uses a centered maximum width (1400px), with total horizontal gutters (104px) on wide screens, (64px) at or below (1100px), and (40px) at or below (760px). Section spacing commonly uses (48–88px), while controls and content groups use the smaller frontmatter rhythm.

The approved landing hero has a (52%/48%) split: headline and actions at left, cyan SQL specimen at right, followed by a full-width installation strip. At or below (760px), it stacks copy before SQL; the client grid changes from four columns to two, benefits and major content columns become single columns, and footer/closing content stacks. Preserve the relationship of each heading to its own content rather than shrinking the desktop page uniformly. At or below (1100px), buttons and column gaps tighten before the main stack occurs.

Documentation keeps Starlight's reading shell: content width (48rem), left navigation from (50rem), and a separate right contents rail from (72rem). Below those thresholds, navigation and contents use the existing mobile controls. Keep tables, commands and code overflow inside their own regions rather than widening the page.

## Elevation & Depth

The landing page has no box shadows: tonal fields, whitespace and one-pixel borders provide separation. Documentation retains Starlight's existing shadows for previous/next cards and the search dialog, with a blurred backdrop only behind that dialog. These bounded reading-shell exceptions do not turn landing cards into floating surfaces. Exact shadow values and transition details live in the sidecar.

**The Quiet Surface Rule.** Use tone and rules for ordinary grouping; preserve shadows for the existing documentation navigation and modal roles.

## Shapes

Controls and client options use gently rounded corners from the control radius. Code examples have the slightly larger code-panel radius; documentation navigation rows use the compact radius. Broad landing color fields and ruled ledgers stay rectangular. Circular pale icon discs belong to benefit illustrations, not to every control. Preserve the supplied B asset, simple inline SVG arrows and the recognizable client marks.

## Components

### Buttons

Primary actions are solid cyan with white text; secondary actions use ink text and a thin cyan border. Both center labels in an inline flex row and share the control radius. Ordinary buttons have a minimum height (58px); the paired hero actions use equal minimum heights (62px), larger labels (22px) and a brighter built hero fill. At the compact desktop breakpoint, minimum heights become (52px). Keep the hero pair vertically aligned.

Hover darkens ordinary primary actions and adds pale cyan to secondary actions. The landing focus treatment is an offset, three-pixel dark-cyan outline; documentation uses its active accent. Landing color transitions take (160ms ease-out) and are removed for reduced-motion users.

### Client selection and installation

Client options are bordered tiles with recognizable SVG marks, bold labels and a pale-cyan selected state. Native links lead to guides without JavaScript; enhancement adds tab roles, selection, arrow/Home/End keys, and switches the command and example together. Do not break this fallback. The installation strip keeps the command horizontally scrollable, the copy control visible, and copy feedback available to assistive technology; its text label hides on small screens.

### Code and ruled content

Landing examples are white bordered panels with a subdued filename strip and internally scrollable code. Benefits use open columns with quiet separating rules; type and integration lists use horizontal ledger lines rather than individual cards. The cyan SQL specimen presents its result in a simple outlined two-row container. Keep its exact result and explanatory caption readable as text.

### Documentation navigation and search

Retain Starlight's native expandable sidebar, cyan current-page treatment, theme selector and previous/next cards. Links are visibly underlined in reading content. Local Pagefind search uses a bordered search field inside the existing dialog, with cyan focus feedback, keyboard opening and a full-screen mobile presentation. Search is a documentation tool, not a new remote service.

## Do's and Don'ts

### Do:

- **Do** preserve the B icon, cyan identity and self-hosted type families.
- **Do** retain real selectable code, visible focus and localized horizontal overflow.
- **Do** use pale fills and thin rules to organize technical content.
- **Do** verify landing reflow and both documentation themes when extending shared styles.
- **Do** keep experimental status and product limitations plainly readable.

### Don't:

- **Don't** replace inline SVG arrows or client marks with text glyph icons.
- **Don't** add decorative shadows to ordinary landing surfaces.
- **Don't** turn every content group into a rounded card or pill.
- **Don't** invent customer, performance, adoption or production-readiness claims.
