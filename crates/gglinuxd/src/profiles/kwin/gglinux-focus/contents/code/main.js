// SPDX-License-Identifier: GPL-3.0-or-later
// gglinux focus: tells gglinuxd which window has the focus, so profiles whose rules act on
// focus can follow it. Each time a window is activated it calls the daemon on the session bus
// with the window's class and process id (an empty class when no window has the focus). If
// gglinuxd isn't running, the call just goes nowhere.

const SERVICE = "io.github.ykk.gglinux.Focus";
const PATH = "/io/github/ykk/gglinux/Focus";
const INTERFACE = "io.github.ykk.gglinux.Focus";

function report(window) {
    if (!window) {
        callDBus(SERVICE, PATH, INTERFACE, "WindowActivated", "", 0);
        return;
    }
    callDBus(SERVICE, PATH, INTERFACE, "WindowActivated", String(window.resourceClass), window.pid || 0);
}

// Plasma 6 names them windowActivated and activeWindow; Plasma 5, clientActivated and
// activeClient.
if (workspace.windowActivated) {
    workspace.windowActivated.connect(report);
    report(workspace.activeWindow);
} else {
    workspace.clientActivated.connect(report);
    report(workspace.activeClient);
}
