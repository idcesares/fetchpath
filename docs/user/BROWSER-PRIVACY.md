# Fetchpath browser extension privacy

Updated 28 September 2026. This page describes the Fetchpath extension for
Chrome and Microsoft Edge. It works with the Fetchpath app installed on your
Windows computer.

## What the extension uses

- When you choose **Send link**, **Send page**, or **Send video**, the extension
  reads the selected HTTP or HTTPS address, a referring page on the same site
  when available, and the cookies your browser would use for that site. It
  sends them to the Fetchpath native host on this
  computer so the app can try the download. The browser asks for permission
  for that site first. The app does not promise to use those cookies for video
  extraction.
- **Send downloads to Fetchpath automatically** is off until you turn it on.
  Turning it on asks for download access and HTTP/HTTPS site access. For a
  browser download it can handle, the extension sends its address and
  applicable cookies to the native host. If the native host refuses the
  handoff, the extension tries to start the browser download again.
- The extension keeps your excluded sites, automatic-mode choice, and up to
  100 recent handoff results in browser-local storage. Each result includes a
  URL with its query and fragment removed, but its path remains and may still
  be sensitive. It records a cookie count, not cookie names or values.

The extension has no advertising or analytics code and sends no information to
a server operated by the Fetchpath project. The native host protects the full
address and cookies with Windows current-user encryption before keeping them
for a download or retry. Fetchpath contacts the website you selected to get
the file; that website handles requests under its own privacy terms. See the
[user guide](GUIDE.md#sending-links-from-your-browser) for the handoff and
[uninstall](GUIDE.md#uninstalling) options.

You can turn off automatic mode in the extension popup, exclude a site with
its page menu, remove a site's browser permission, or remove the extension in
your browser. Removing the extension clears its browser-local settings and
recent results. Fetchpath's saved jobs and protected request context are part
of the app's data; uninstall offers to remove that data.
