// Melissa's own script: small conveniences on top of htmx. Served from this origin (the CSP
// forbids inline code); no eval, no markup building (file chips are copies of a server-rendered
// template). Everything works without it, except Enter-to-send, automatic scrolling, pasting
// or dropping files and previews of picked files (the attach button still works).
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
    if ((box.value.trim() !== "" || kept.length > 0) && box.form) {
      box.form.requestSubmit();
    }
  });

  // --- The message box grows with its text (CSS `field-sizing` where supported). -----------

  var growsByItself = !!(window.CSS && CSS.supports && CSS.supports("field-sizing", "content"));

  function grow(box) {
    if (growsByItself || !box) {
      return;
    }
    box.style.height = "auto";
    box.style.height = box.scrollHeight + "px";
  }

  document.addEventListener("input", function (e) {
    if (e.target === composer()) {
      grow(e.target);
    }
  });

  // --- Files to send: picked with the paperclip, pasted, or dropped on the composer. --------
  // `kept` is the list; the file input carries it when the form is sent.

  var kept = [];
  var previews = [];
  var IMAGE_TYPES = ["image/png", "image/jpeg", "image/gif", "image/webp"];

  function attachInput() {
    return document.getElementById("attach");
  }

  function sizeText(n) {
    if (n >= 1048576) {
      return (n / 1048576).toFixed(1) + " MiB";
    }
    if (n >= 1024) {
      return Math.round(n / 1024) + " KiB";
    }
    return n + " B";
  }

  function note(text) {
    var p = document.getElementById("composer-note");
    if (p) {
      p.textContent = text;
      p.hidden = text === "";
    }
  }

  // Put `kept` into the file input (what the form sends).
  function sync() {
    var input = attachInput();
    if (!input || !window.DataTransfer) {
      return;
    }
    var dt = new DataTransfer();
    kept.forEach(function (f) {
      dt.items.add(f);
    });
    input.files = dt.files;
  }

  function clearPreviews() {
    previews.forEach(function (url) {
      URL.revokeObjectURL(url);
    });
    previews = [];
  }

  function showFiles() {
    var list = document.getElementById("composer-files");
    var template = document.getElementById("file-chip");
    if (!list || !template || !template.content) {
      return;
    }
    clearPreviews();
    list.replaceChildren();
    kept.forEach(function (f, i) {
      var chip = template.content.firstElementChild.cloneNode(true);
      chip.querySelector(".name").textContent = f.name;
      chip.querySelector(".size").textContent = sizeText(f.size);
      var remove = chip.querySelector(".remove");
      remove.dataset.index = String(i);
      remove.setAttribute("aria-label", "Remove " + f.name);
      if (IMAGE_TYPES.indexOf(f.type) >= 0) {
        var url = URL.createObjectURL(f);
        previews.push(url);
        var img = chip.querySelector(".thumb");
        img.src = url;
        img.hidden = false;
      }
      list.appendChild(chip);
    });
  }

  // Add files within the limits the input states; say why any were left out.
  function addFiles(files) {
    var input = attachInput();
    if (!input || !window.DataTransfer) {
      return;
    }
    var maxFile = Number(input.dataset.maxFile);
    var maxTotal = Number(input.dataset.maxTotal);
    var maxCount = Number(input.dataset.maxCount);
    var total = kept.reduce(function (sum, f) {
      return sum + f.size;
    }, 0);
    var refused = "";
    Array.prototype.forEach.call(files, function (f) {
      if (f.size > maxFile) {
        refused = f.name + " is over " + sizeText(maxFile) + ".";
      } else if (kept.length >= maxCount) {
        refused = "At most " + maxCount + " files per message.";
      } else if (total + f.size > maxTotal) {
        refused = "At most " + sizeText(maxTotal) + " of files per message.";
      } else {
        kept.push(f);
        total += f.size;
      }
    });
    note(refused);
    sync();
    showFiles();
  }

  document.addEventListener("change", function (e) {
    if (e.target === attachInput()) {
      // The picker replaces the input's list: take the new files, keep the earlier ones.
      var fresh = Array.prototype.slice.call(e.target.files);
      sync();
      addFiles(fresh);
    }
  });

  document.addEventListener("paste", function (e) {
    var data = e.clipboardData;
    if (e.target !== composer() || !data || !data.files || data.files.length === 0) {
      return;
    }
    // A screenshot pastes as a file only; keep pasted text (e.g. a copied file name) as text.
    if (Array.prototype.indexOf.call(data.types, "text/plain") < 0) {
      e.preventDefault();
    }
    addFiles(data.files);
  });

  function draggingFiles(e) {
    var form = e.target.closest ? e.target.closest("#composer") : null;
    var types = e.dataTransfer && e.dataTransfer.types;
    return form && types && Array.prototype.indexOf.call(types, "Files") >= 0 ? form : null;
  }

  document.addEventListener("dragover", function (e) {
    var form = draggingFiles(e);
    if (form) {
      e.preventDefault();
      form.classList.add("dropping");
    }
  });

  document.addEventListener("dragleave", function (e) {
    var form = draggingFiles(e);
    if (form && !form.contains(e.relatedTarget)) {
      form.classList.remove("dropping");
    }
  });

  document.addEventListener("drop", function (e) {
    var form = draggingFiles(e);
    if (form) {
      e.preventDefault();
      form.classList.remove("dropping");
      addFiles(e.dataTransfer.files);
    }
  });

  document.addEventListener("click", function (e) {
    var remove = e.target.closest ? e.target.closest("#composer-files .remove") : null;
    if (!remove) {
      return;
    }
    kept.splice(Number(remove.dataset.index), 1);
    note("");
    sync();
    showFiles();
    var box = composer();
    if (box) {
      box.focus();
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
    // Logs: show the newest lines first (the end of the tail).
    var log = document.querySelector("#app-logs .log");
    if (log) {
      log.scrollTop = log.scrollHeight;
    }
  });

  // After sending, the composer is replaced: put the cursor back in it.
  document.addEventListener("htmx:afterSwap", function (e) {
    if (e.detail && e.detail.target && e.detail.target.id === "composer") {
      // Sent: the new composer starts empty. Refused (busy, ...): keep the files for a retry.
      if (document.querySelector("#composer > .composer-error:not(#composer-note)")) {
        sync();
        showFiles();
      } else {
        kept = [];
        clearPreviews();
      }
      var box = composer();
      if (box) {
        grow(box);
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
    grow(composer());
    if (document.getElementById("feed")) {
      scrollToBottom();
    }
  });
})();
