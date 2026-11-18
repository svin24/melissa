// Melissa's own script: small conveniences on top of htmx. Served from this origin (the CSP
// forbids inline code); no eval, no markup building. Everything works without it, except
// Enter-to-send and automatic scrolling.
(function () {
  "use strict";

  var stickToBottom = true;

  function nearBottom() {
    return window.innerHeight + window.scrollY >= document.documentElement.scrollHeight - 200;
  }

  function scrollToBottom() {
    window.scrollTo(0, document.documentElement.scrollHeight);
  }

  function composer() {
    return document.getElementById("message");
  }

  // Enter sends, Shift+Enter starts a new line (not while an input method is composing).
  document.addEventListener("keydown", function (e) {
    var box = composer();
    if (e.target !== box || e.key !== "Enter" || e.shiftKey || e.isComposing) {
      return;
    }
    e.preventDefault();
    if (box.value.trim() !== "" && box.form) {
      box.form.requestSubmit();
    }
  });

  document.addEventListener("htmx:sseBeforeMessage", function (e) {
    if (e.detail && e.detail.type === "reload") {
      window.location.reload();
      return;
    }
    stickToBottom = nearBottom();
  });

  document.addEventListener("htmx:sseMessage", function () {
    if (stickToBottom && document.getElementById("feed")) {
      scrollToBottom();
    }
  });

  // After sending, the composer is replaced: put the cursor back in it.
  document.addEventListener("htmx:afterSwap", function (e) {
    if (e.detail && e.detail.target && e.detail.target.id === "composer") {
      var box = composer();
      if (box) {
        box.focus();
      }
    }
  });

  document.addEventListener("DOMContentLoaded", function () {
    if (document.getElementById("feed")) {
      scrollToBottom();
    }
  });
})();
