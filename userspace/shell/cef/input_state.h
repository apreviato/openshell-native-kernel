// What the X server says of the person's input: how long the keyboard and
// the mouse have not been touched (MIT-SCREEN-SAVER), for the Desktop's saved
// passwords, which lock after a while without input as Electron's do; and
// where the pointer is, for presses on the pictures the Desktop shows above
// the tabs. Both on a connection of their own, on the browser process UI thread.
#pragma once

namespace openshell {

// Seconds since the last input, or -1 where the display cannot tell.
double IdleSeconds();

// The pointer, in screen pixels. False where the display cannot tell.
bool PointerOnScreen(int& x, int& y);

}  // namespace openshell
