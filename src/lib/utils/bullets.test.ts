import assert from "node:assert/strict";
import { afterInput, onEnter, onTab, toggleBullet } from "./bullets";

// "- " and "* " at the start of a line become bullets; elsewhere they don't.
assert.deepEqual(afterInput("- ", 2), { text: "• ", cursor: 2 });
assert.deepEqual(afterInput("a\n  * ", 6), { text: "a\n  • ", cursor: 6 });
assert.equal(afterInput("a - ", 4), null);

// Enter carries the list on.
assert.deepEqual(onEnter("• milk", 6), { text: "• milk\n• ", cursor: 9 });
assert.deepEqual(onEnter("  • milk", 8), {
  text: "  • milk\n  • ",
  cursor: 13,
});
assert.deepEqual(onEnter("1. one", 6), { text: "1. one\n2. ", cursor: 10 });
// Enter on an empty item ends the list.
assert.deepEqual(onEnter("• milk\n• ", 9), { text: "• milk\n", cursor: 7 });
// Not a list: Enter is left alone.
assert.equal(onEnter("plain", 5), null);

// Tab indents an item; Shift+Tab takes it back.
assert.deepEqual(onTab("• a", 3, false), { text: "  • a", cursor: 5 });
assert.deepEqual(onTab("  • a", 5, true), { text: "• a", cursor: 3 });
assert.equal(onTab("plain", 2, false), null);

// The button turns a line into an item and back.
assert.deepEqual(toggleBullet("a\nb", 3), { text: "a\n• b", cursor: 5 });
assert.deepEqual(toggleBullet("a\n• b", 5), { text: "a\nb", cursor: 3 });

console.log("bullets ok");
