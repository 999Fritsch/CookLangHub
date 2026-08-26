/*
 * Interactive timers for a Recipe.
 *
 * The server writes every timer as plain words with its length, and it puts
 * the length in seconds on the badge. This file turns each of those badges
 * into a button that counts down. If the file does not run, the badge stays
 * the words that the author wrote, so a cook still reads the time.
 *
 * The button keeps the CookCLI `timer-badge` class and the CookCLI
 * stopwatch mark, so a running timer is the same component in another
 * state. This file is new work and not a copy of a CookCLI file.
 *
 * A timer that ends must be noticeable. A browser blocks a sound that
 * starts on its own, so the signal is a change a person can see: the badge
 * reads `Time is up` and pulses, and the tab says so for a cook who looked
 * away.
 *
 * Nothing here writes to the server. A timer is a kitchen tool and not a
 * change to the Recipe.
 */
(function () {
  "use strict";

  var TICK_MS = 250;
  var DONE_CLASSES = ["animate-pulse", "font-bold"];
  var IDLE_CLASSES = ["timer-badge", "cursor-pointer"];

  var pageTitle = document.title;
  var announcer = null;
  var finished = 0;

  /** One place where a screen reader hears that a timer ended. */
  function announce(message) {
    if (!announcer) {
      announcer = document.createElement("p");
      announcer.className = "sr-only";
      announcer.setAttribute("role", "status");
      document.body.appendChild(announcer);
    }
    announcer.textContent = message;
  }

  /** Show in the tab that a timer ended, for a cook who looked away. */
  function markTitle(delta) {
    finished = Math.max(0, finished + delta);
    document.title = finished > 0 ? "Time is up · " + pageTitle : pageTitle;
  }

  /** Seconds as a clock: 4:05, or 1:02:03 for an hour or more. */
  function clock(total) {
    var seconds = Math.max(0, Math.round(total));
    var hours = Math.floor(seconds / 3600);
    var minutes = Math.floor((seconds % 3600) / 60);
    var rest = seconds % 60;
    var pad = function (value) {
      return value < 10 ? "0" + value : String(value);
    };
    if (hours > 0) {
      return hours + ":" + pad(minutes) + ":" + pad(rest);
    }
    return minutes + ":" + pad(rest);
  }

  /** Make one badge a button that counts down. */
  function build(badge) {
    var total = parseInt(badge.getAttribute("data-timer-seconds"), 10);
    var label = badge.getAttribute("data-timer-label") || "";
    if (!isFinite(total) || total < 1) {
      return;
    }

    var button = document.createElement("button");
    button.type = "button";
    button.className = IDLE_CLASSES.join(" ");

    var hidden = document.createElement("span");
    hidden.className = "sr-only";

    var icon = document.createElement("span");
    icon.setAttribute("aria-hidden", "true");
    icon.textContent = "⏱️";

    var words = document.createTextNode("");

    button.appendChild(hidden);
    button.appendChild(icon);
    button.appendChild(document.createTextNode(" "));
    button.appendChild(words);

    var ticker = null;
    var deadline = 0;
    var state = "idle";

    function paint(text, spoken) {
      words.nodeValue = text;
      hidden.textContent = spoken;
    }

    function idle() {
      state = "idle";
      DONE_CLASSES.forEach(function (name) {
        button.classList.remove(name);
      });
      paint(label, "Time: ");
      button.setAttribute("aria-label", "Start the timer: " + label);
    }

    function done() {
      state = "done";
      window.clearInterval(ticker);
      ticker = null;
      DONE_CLASSES.forEach(function (name) {
        button.classList.add(name);
      });
      paint("Time is up", "Timer: ");
      button.setAttribute("aria-label", "Time is up. Reset the timer: " + label);
      announce("The timer for " + label + " is finished.");
      markTitle(1);
    }

    function tick() {
      var left = (deadline - Date.now()) / 1000;
      if (left <= 0) {
        done();
        return;
      }
      // The clock reads from a deadline, so a tab that the browser slowed
      // down still shows the true time that is left.
      paint(clock(left), "Timer: ");
    }

    function start() {
      state = "running";
      deadline = Date.now() + total * 1000;
      button.setAttribute("aria-label", "Stop the timer: " + label);
      paint(clock(total), "Timer: ");
      ticker = window.setInterval(tick, TICK_MS);
    }

    function stop() {
      window.clearInterval(ticker);
      ticker = null;
      idle();
    }

    button.addEventListener("click", function () {
      if (state === "running") {
        stop();
      } else if (state === "done") {
        markTitle(-1);
        idle();
      } else {
        start();
      }
    });

    idle();
    badge.parentNode.replaceChild(button, badge);
  }

  var badges = document.querySelectorAll("[data-timer-seconds]");
  for (var i = 0; i < badges.length; i++) {
    build(badges[i]);
  }
})();
