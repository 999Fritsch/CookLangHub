/*
 * Show a Recipe at another serving count or in other units, at once.
 *
 * CookCLI changes the view the moment the number changes. It does this with
 * an `onchange` attribute on the input, which the Content Security Policy of
 * this application does not allow, so the same behaviour lives in this file.
 *
 * The form works without this file: it is a plain GET form with a Show
 * button. This only removes the need to press the button. The button stays
 * for anybody whose browser does not run this, and it is hidden only after
 * this file has run and can do the work instead.
 *
 * A serving count is typed digit by digit, so sending the form on every
 * keystroke would leave the cook on a page for "1" while they type "12".
 * A number therefore waits until typing stops, and a menu does not wait.
 */
(function () {
  "use strict";

  var form = document.querySelector("[data-view-options]");
  if (!form) return;

  var button = form.querySelector("[data-view-options-submit]");
  if (button) {
    // Hidden rather than removed, so the form still has a way to be sent
    // if a browser refuses to run the submit below.
    button.hidden = true;
  }

  var waiting = null;

  function show() {
    window.clearTimeout(waiting);
    waiting = null;

    // A serving count of nothing, or one the field itself calls wrong,
    // must not reach the address. Leave the page as it is.
    if (!form.checkValidity()) return;

    form.submit();
  }

  form.addEventListener("change", function (event) {
    if (event.target.type === "number") {
      // A change event on a number also arrives on blur and on a step, and
      // the pause below has usually sent the form already.
      show();
    } else {
      show();
    }
  });

  form.addEventListener("input", function (event) {
    if (event.target.type !== "number") return;
    window.clearTimeout(waiting);
    waiting = window.setTimeout(show, 600);
  });
})();
