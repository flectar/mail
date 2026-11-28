# Design system

Flectar Mail's controls are one system. A button, a text field, a select and a
row of tabs share their sizes, corners, edges, type and focus ring, so they look
like they belong together wherever they meet. The values follow
[shadcn/ui](https://ui.shadcn.com) (its `new-york-v4` components), drawn with
the app's own theme colors, so every preset and custom theme keeps working.

The values live in `ui/design-system.slint`, the controls in `ui/components/`.
Views pick a control and a variant, never their own colors, corners or heights.
[`design-system/gallery.slint`](design-system/gallery.slint) shows every control
from the real components. Open it with
`slint-viewer docs/design-system/gallery.slint`, and set its `dark` property to
see the dark theme.

## Values

| | Value | In `ui/design-system.slint` |
| --- | --- | --- |
| Small control | 32 px tall | `LayoutMetrics.small-control-height` |
| Control | 36 px tall | `LayoutMetrics.medium-control-height` |
| Large control | 40 px tall | `LayoutMetrics.control-height` |
| Phone control | 44 px tall | `LayoutMetrics.touch-target` |
| Corner base | 10 px | `LayoutMetrics.corner-base` |
| Control corners | 8 px, four fifths of the base | `LayoutMetrics.corner-radius` |
| Card and tab track corners | 10 px, the base | `LayoutMetrics.container-radius` |
| Small corners, such as a menu row | 6 px, three fifths of the base | `LayoutMetrics.small-radius` |
| Button label room | 12 px each side | `LayoutMetrics.button-padding` |
| Field and select text room | 12 px each side | `LayoutMetrics.field-padding` |
| Edge of a button | 1 px of the text color at 13%, white at 12% in dark mode | `AppTheme.control-edge` |
| Edge of a field or select | 1 px `input-border` | `AppTheme.input-border` |
| Face of a button | lit from above, a step darker under the pointer | `AppTheme.control-face-top`, `control-face-bottom`, `control-hover-top`, `control-hover-bottom` |
| Neutral fills, faint to strong | the text color over the surface at 4.5%, 7.5% and 10.5% (5%, 8% and 11% in dark mode) | `AppTheme.fill-subtle`, `fill-hover`, `fill-pressed` |
| Under the pointer: a row, a ghost button, the strip's buttons, and a tab track | `fill-hover` | `AppTheme.muted-control-bg` |
| Toggle that is on, a selected list row | `fill-pressed` | `AppTheme.control-on`, `selected-row` |
| Shadow under an outlined control | 1 px down, 2 px blur, 6% black, light theme only | `AppTheme.control-shadow` |
| Shadow under a raised control, such as the chosen tab | 1 px down, 3 px blur, 12% black, light theme only | `AppTheme.raised-shadow` |
| Shadow under a primary or destructive button | its own color at 28%, light theme only | `AppTheme.primary-shadow`, `destructive-shadow` |
| A menu, and a select's list | the surface, a step lighter in dark mode, with a shadow of 16 px blur, 6 px down, in both themes | `AppTheme.popover-bg`, `popover-shadow` |
| A menu's highlighted row | `fill-hover` over the menu's own color | `AppTheme.popover-hover` |
| Destructive text, such as a menu's Move to trash | `destructive`, lighter in dark mode for 4.5:1 | `AppTheme.destructive-text` |
| Keyboard focus ring | the edge turns `accent`, with a 3 px halo of it outside | `FocusRing`, `LayoutMetrics.focus-ring-width`, `AppTheme.focus-ring`, `focus-halo` |
| Labels of buttons and tabs | 14 px at the default text size, medium weight | `TypeScale.body`, weight 500 |
| Values in fields and selects | 14 px, regular weight | `TypeScale.body` |

In the dark theme shadows don't read, so a control's border marks its edge on
its own. A disabled control keeps its look at half opacity.

The neutral fills come from one scale, so a hovered row, a hovered toolbar
button and a tab track are the same gray everywhere. Being the text color over
the surface, they follow every palette and both themes. Use them, or the names
built on them, rather than a gray of a control's own.

Every corner follows the corner base, so one value makes the whole app squarer
or rounder, as shadcn derives its corners from one `--radius`. Things round by
nature stay round, such as avatars, a switch's track and a radio button, with
`self.height / 2`. `scripts/check-design-corners.py` fails the build when a
file gains a corner written in fixed pixels, and the allowance in
`scripts/design-corners-baseline.json` only goes down as areas move over.

## Controls

**Button** (`button.slint`) is outlined by default, for most actions, with a
face lit from above. `primary` fills it with the brand color for the main action
of a view or a form, such as New email, New event or Save. Each view or form has
one primary button at most. `destructive` is for removing something, and
`selected` marks a toggle that is on, such as Bold, or the chosen one of a few
options, which looks pushed in. A button is 36 px tall, 32 px in a dense row and
44 px on a phone. Give it room for its whole label with
`min-width: max(64px, self.content-width)`.

**Icon button** (`controls.slint`) is ghost by default, with no fill until the
pointer is on it, for toolbar actions such as Previous and Next. `primary` fills
it, as for the floating New email button. Every icon button has a tooltip.

**Tabs** (`tabs.slint`) switch between views or sections, such as Week, Month
and Agenda, or the sections of an account's settings. The chosen tab is raised
on a muted track. When the labels don't fit side by side, the tabs stack, unless
`stack-when-narrow` is off for a short set that always fits. Actions stay
outside the tabs. Today moves whichever view is shown to today, so it's a button
beside them. The arrow keys move to the next or previous tab and choose it, and
Home and End go to the first and last, as the
[WAI-ARIA tabs pattern](https://www.w3.org/WAI/ARIA/apg/patterns/tabs/) has it.

**Select** (`select.slint`) chooses one value from a list. It looks like a text
field with a chevron, and its list looks like the app's menus. The keys follow
the
[WAI-ARIA select-only combobox](https://www.w3.org/WAI/ARIA/apg/patterns/combobox/examples/combobox-select-only/).
Down, Alt+Down, Return and Space open the list on the chosen value, Up and Home
on the first value, and End on the last. In the list the arrows, Home, End,
Page Up and Page Down move, Return, Space and Alt+Up choose, Tab chooses and
moves on, and Escape closes the list and keeps the value. The value changes only
when one is chosen, so moving through the list or scrolling over it never saves
anything. Finding a value by typing its first letters isn't there yet.

**Text field** (`LineEdit` in `controls.slint`) and **text area** (`TextEdit`
in `controls.slint`) take typed text. A text area is at least 80 px tall.

**Search fields** are text fields with `filled`: a soft `fill-subtle` with no
edge at rest, which becomes an outlined field while it has the focus, as the
search fields of macOS, Android and GNOME do. The list's search is 36 px in its
toolbar, with the filter button inside it; a sidebar's is 32 px.

**Menu** (`PopupMenu` in `popup-menu.slint`) is the app's one menu, for a
button's menu and a right-click menu alike. The app draws it, so it looks the
same on every platform. It follows shadcn's DropdownMenu: 4 px inside a 1 px
edge with the control corners, 32 px rows (44 px on a phone) with small
corners, 14 px titles, checks and icons in a 16 px column before the titles,
a line between groups, and destructive entries in red.

- Its entries are data, `PopupMenuEntry`. An entry that only applies
  sometimes is `hidden`, so one list covers every case. A line separates two
  shown entries whose `group` differs. `model` and `model-titles` add entries
  from a model after the fixed ones, and `trailing` comes after them.
- A submenu is a page. An entry with a `submenu` opens the entries whose
  `parent` names it, in place of the menu and under a Back row, as phone menus
  show them.
- Put the menu outside a layout. A button opens it with `open-below`, a button
  at the bottom of the window, such as a phone's floating button, with
  `open-above`, and a right click with `open-at`. It opens above its button
  when there's no room below, from the height each window keeps in
  `PopupMenuSpace`.
- The keys follow the
  [WAI-ARIA menu pattern](https://www.w3.org/WAI/ARIA/apg/patterns/menubar/).
  The arrows move and wrap around, Home and End go to the ends, Return and
  Space choose, Right opens a page and Left goes back, and Escape and Tab
  close it. Opened from the keyboard, it highlights its first entry. A
  disabled entry can be highlighted but not chosen.

**Badge** (`badge.slint`) shows a short status or count beside what it
describes, such as a folder's unread count, This device or Coming soon. As
shadcn's Badge, it has the control corners and 12 px medium text, and it's
20 px tall. `neutral` is a count or a plain fact, `accent` what's current or
new, `solid` a strong label such as a calendar's provider, and `destructive` a
problem. The accent tint is see-through, so a selected row's count still
shows.

**Switch** turns a setting on or off at once. **Checkbox** (`CheckBox`, with its
label) is for a form saved with a button, `SelectionCheckBox` chooses items in a
list, and **radio buttons** choose one of a few options shown together.
Checkboxes and radio buttons draw their edge in `AppTheme.secondary`, which is
darker than `input-border`, so they keep the 3:1 contrast WCAG asks of a
control's edge.

**Settings rows** (`settings-components.slint`) put a select or a switch on
their title's line, with a description under it.

## Sidebars

Mail, Calendar, Files, Contacts and Settings build their sidebars from the
same parts, in `sidebar-row.slint` and `sidebar-controls.slint`, with the
measures in `SidebarStyle`. Every sidebar has the same background.

- The top holds the workspace's primary button, such as New email, and
  `SidebarSearch` when the sidebar can be searched. Settings has no primary
  button, so its search is at the top.
- `SidebarGroupLabel` names a group in 11 px small capitals in the secondary
  text color: ALL ACCOUNTS for the rows that span every account, ACCOUNTS above
  the accounts, MY CALENDARS and SETTINGS. It keeps 12 px above it after
  another group and 12 px between its text and the first row, so it reads as
  part of the rows below. An action that adds to the group, such as the + that
  adds an account, sits at the end of its label.
- A row (`SidebarItem`, or `SidebarRowSurface` for rows without a folder's menu)
  is 32 px tall, with 2 px between rows, and 44 px in a phone's drawer. Its
  18 px icon sits after a 22 px gutter, which holds a folder's disclosure arrow,
  and its label starts 30 px after that. A count sits in a badge at the end.
  Hover is a neutral fill, and the one selected row takes the soft accent and a
  semibold label. Rows inside another row, such as subfolders or a storage's
  spaces, indent by 16 px.
- An account heads its rows with its picture or initials, its name and its
  address (`SidebarSectionHeader`, or `SidebarAccountHeading` where the rows
  don't fold).
- Icons take their own color, such as yellow for Starred, unless Settings'
  "Monochrome sidebar icons" is on. Then they are muted, and the selected row's
  icon takes the accent. `SidebarStyle.icon-color` applies that rule. A
  calendar's color shows in its checkbox in both modes, since it's the key to
  the calendar's events.
- Rows end where the primary button ends. A list reaches into the sidebar's
  12 px margin, so its scroll bar, which Slint draws over a list's edge, sits
  there instead of over the rows' counts.
- Actions that aren't places, such as Files' Sharing activity and Transfers,
  sit in a foot under a divider. A storage shows how full it is there, with a
  bar and its sizes.

## On phones

- Controls are 44 px tall, and a row of buttons stacks into full-width buttons
  whose labels wrap rather than get cut.
- Tabs stack when their labels don't fit.
- Each workspace's create action, such as New email, is the floating button in
  the bottom corner.

## Don't

- Don't use the `ComboBox`, `TextEdit` or `CheckBox` of Slint's `std-widgets`
  for a control people see. They draw themselves in Slint's style, not the
  app's. Layouts such as `VerticalBox`, and `ScrollView` and `ListView`, are
  fine.
- Don't use Slint's `ContextMenuArea` or `Menu`. Windows and macOS draw
  them natively, so they don't look like the app, and they don't scroll. The
  system tray's menu is the one the system draws.
- Don't give a view its own colors, corners or control heights. Add a value to
  `ui/design-system.slint` when a control needs one.
- Don't put a shadow, an opacity, `visible` or an animated color on the root
  of a control. Slint then copies the whole control into each place it's used,
  which grew the generated UI by a quarter and ran the release build out of
  memory. Draw the control on an inner face instead, as `Button`, `LineEdit` and
  the others do, so Slint builds it once.
