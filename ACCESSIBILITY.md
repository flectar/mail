# Accessibility

Flectar Mail aims to make email, calendars, contacts, and files usable with
keyboard navigation, readable layouts, and assistive technology. Accessibility
work is ongoing while the app is in development.

## Current features

- Keyboard navigation for the message list and reader, including arrow keys,
  Home/End, Page Up/Page Down, Enter, Space, and F6.
- Keyboard shortcuts for tabs, with the available shortcuts listed in Settings.
- Full and minimal desktop layouts, plus a compact layout for smaller screens.
- Light and dark themes, built-in and custom color palettes, and independent
  message appearance settings. Reader mode provides another way to read mail.
- Accessibility labels, roles, and actions on controls such as navigation,
  tabs, and calendar buttons.

See the [README](README.md) for navigation instructions and appearance options.

## Limitations and testing

These features do not establish complete accessibility. Screen-reader support
and keyboard access still need end-to-end testing across desktop and mobile
platforms. This document does not claim conformance with an accessibility
standard or verified compatibility with any particular screen reader.

HTML email rendering is experimental. Complex messages may display
incorrectly, and sender-provided content can introduce additional barriers.
Custom color palettes and original message colors may also reduce contrast.

## Report an accessibility barrier

[Open an issue](https://github.com/flectar/mail/issues/new) with
“Accessibility” in the title. Include, where possible:

- The Flectar Mail version, operating system, and desktop environment.
- Assistive technology and its version, if used.
- The task you were trying to complete, steps to reproduce the barrier, and
  what you expected to happen.
- Relevant display settings, such as scaling, theme, or color palette.

Use fictional messages and remove private information from screenshots and
logs. If you need to report privately, email
[hello@flectar.com](mailto:hello@flectar.com). You do not need to disclose a
disability or medical information to report a barrier. Security vulnerabilities
should follow the [security policy](SECURITY.md).

## Contributing

When changing the interface, check keyboard operation, visible focus, readable
text and contrast in light and dark themes, and meaningful labels for controls.
Avoid relying on color alone to communicate state. Check assistive-technology
behavior where possible and describe the environment tested in your pull
request. Documentation images should include useful alternative text.

The repository maintainers track accessibility work through issues and pull
requests. Update this page when accessibility behavior or known limitations
change. See [CONTRIBUTING.md](CONTRIBUTING.md) for the contribution workflow.
