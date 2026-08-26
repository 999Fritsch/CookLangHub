/*
 * Copy the address of a Recipe.
 *
 * The page shows the address in a field that a person can select and copy by
 * hand, so Share works with no script at all. This file adds the one thing
 * that a page cannot do on its own: put the address on the clipboard with one
 * action. The button stays hidden until this file runs, so a person never
 * sees a control that does nothing.
 *
 * The file is served from this host. The Content Security Policy is
 * `default-src 'self'`, and this page must keep working under it.
 */
(function () {
    "use strict";

    var field = document.getElementById("recipe-address");
    var button = document.getElementById("copy-recipe-address");
    var result = document.getElementById("copy-recipe-address-result");

    if (!field || !button) {
        return;
    }

    button.classList.remove("hidden");

    function say(message) {
        if (result) {
            result.textContent = message;
        }
    }

    button.addEventListener("click", function () {
        field.focus();
        field.select();

        if (navigator.clipboard && navigator.clipboard.writeText) {
            navigator.clipboard.writeText(field.value).then(
                function () {
                    say("The address is on the clipboard.");
                },
                function () {
                    say("The browser refused to copy. Copy the selected address.");
                }
            );
            return;
        }

        // An older browser, or a page that the browser does not treat as
        // secure. The address is selected, so a person can still copy it.
        var copied = false;
        try {
            copied = document.execCommand("copy");
        } catch (error) {
            copied = false;
        }

        say(
            copied
                ? "The address is on the clipboard."
                : "The browser refused to copy. Copy the selected address."
        );
    });
})();
