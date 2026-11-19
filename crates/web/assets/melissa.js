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

  // App details: a Details button loads the dialog's content; open it as a modal. The
  // browser handles Escape, focus containment and returning focus to the button.
  document.addEventListener("htmx:afterSwap", function (e) {
    var dialog = e.detail && e.detail.target;
    if (dialog && dialog.id === "app-dialog" && !dialog.open && dialog.showModal) {
      dialog.showModal();
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

  // Copy buttons on code blocks in agent replies. They stay hidden (CSS) unless the
  // clipboard is usable, which needs a secure context (localhost or HTTPS).
  if (navigator.clipboard && window.isSecureContext) {
    document.documentElement.classList.add("can-copy");
  }

  document.addEventListener("click", function (e) {
    var button = e.target.closest ? e.target.closest(".copy-code") : null;
    if (!button) {
      return;
    }
    var code = button.parentElement && button.parentElement.querySelector("pre code");
    if (!code || !navigator.clipboard) {
      return;
    }
    navigator.clipboard.writeText(code.textContent).then(function () {
      button.textContent = "Copied";
      setTimeout(function () {
        button.textContent = "Copy";
      }, 1500);
    });
  });

  document.addEventListener("DOMContentLoaded", function () {
    if (document.getElementById("feed")) {
      scrollToBottom();
    }
  });
})();
