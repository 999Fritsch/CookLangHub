// Show only the source that the person selected on the create form.
//
// The two modes are exclusive. This file makes that visible: it hides the
// other panel and turns its field off, so the browser sends one source
// only.
//
// Without this file the page still works. Both panels stay visible, the
// radio still says which source applies, and the server refuses a form
// that carries two. Nothing here is a permission, and nothing here is a
// limit: the server checks the size and the format again.
(function () {
  "use strict";

  var form = document.getElementById("recipe-form");
  if (!form) {
    return;
  }

  var modes = form.querySelectorAll('input[name="mode"]');
  var panels = {
    text: document.getElementById("mode-text"),
    file: document.getElementById("mode-file")
  };

  function apply() {
    var chosen = "text";
    Array.prototype.forEach.call(modes, function (radio) {
      if (radio.checked) {
        chosen = radio.value;
      }
    });

    Object.keys(panels).forEach(function (name) {
      var panel = panels[name];
      if (!panel) {
        return;
      }

      var selected = name === chosen;
      panel.hidden = !selected;

      // A field that is off is not sent, which is what keeps the two
      // modes apart.
      Array.prototype.forEach.call(
        panel.querySelectorAll("input, textarea"),
        function (field) {
          field.disabled = !selected;
        }
      );
    });
  }

  Array.prototype.forEach.call(modes, function (radio) {
    radio.addEventListener("change", apply);
  });

  apply();
})();
