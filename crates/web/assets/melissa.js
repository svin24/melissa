// Melissa's own script: small conveniences on top of htmx. Served from this origin (the CSP
// forbids inline code); no eval, no markup building (file chips are copies of a server-rendered
// template). Everything works without it, except Enter-to-send, automatic scrolling, pasting
// or dropping files, previews of picked files (the attach button still works), voice input
// (its button stays hidden), the model picker's filter, search, favorites and shortcuts
// (without script it lists every model and each pick is a plain form post) and the details
// panel's sheet on narrow screens (without script it is a section below the chat).
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
      showVoice();
    }
  });

  // --- Voice input: record here, turn speech into text on the box, put it in the box. -----
  // The browser records (MediaRecorder), decodes its own recording and renders it at 16 kHz
  // mono (OfflineAudioContext), and sends a plain WAV to the button's `data-url`. The text is
  // inserted for editing; nothing is sent by itself. Microphones need a secure context
  // (HTTPS or localhost): elsewhere the button says so instead of failing silently.

  var VOICE_RATE = 16000;
  var VOICE_MAX_MS = 120000;
  var rec = null; // the recording now: {recorder, started, timer}
  var transcribing = false;
  var asking = false; // the browser's microphone prompt is open

  function voiceButton() {
    return document.getElementById("voice");
  }

  function canRecord() {
    return !!(
      window.isSecureContext &&
      navigator.mediaDevices &&
      navigator.mediaDevices.getUserMedia &&
      window.MediaRecorder &&
      window.OfflineAudioContext &&
      (window.AudioContext || window.webkitAudioContext)
    );
  }

  function clock(ms) {
    var s = Math.floor(ms / 1000);
    return Math.floor(s / 60) + ":" + String(s % 60).padStart(2, "0");
  }

  // The button shows the state: ready, recording (with the time) or working.
  function showVoice() {
    var b = voiceButton();
    if (!b) {
      return;
    }
    b.hidden = false;
    var state = rec ? "recording" : transcribing ? "working" : "idle";
    var label = {
      idle: "Speak instead of typing",
      recording: "Stop recording",
      working: "Turning speech into text",
    }[state];
    b.classList.toggle("recording", state === "recording");
    b.classList.toggle("working", state === "working");
    b.setAttribute("aria-pressed", state === "recording" ? "true" : "false");
    b.disabled = state === "working";
    b.title = label;
    var l = b.querySelector(".voice-label");
    if (l) {
      l.textContent = label;
    }
    var t = b.querySelector(".voice-time");
    if (t) {
      t.textContent = rec ? clock(Date.now() - rec.started) : "";
    }
  }

  function startRecording(url) {
    if (!canRecord()) {
      note(
        window.isSecureContext
          ? "This browser can't record audio."
          : "The microphone needs HTTPS or localhost."
      );
      return;
    }
    note("");
    asking = true;
    navigator.mediaDevices.getUserMedia({ audio: true }).then(
      function (stream) {
        asking = false;
        var recorder;
        try {
          recorder = new MediaRecorder(stream);
        } catch (err) {
          stream.getTracks().forEach(function (t) {
            t.stop();
          });
          note("This browser can't record audio.");
          return;
        }
        var chunks = [];
        recorder.addEventListener("dataavailable", function (e) {
          if (e.data && e.data.size > 0) {
            chunks.push(e.data);
          }
        });
        recorder.addEventListener("stop", function () {
          stream.getTracks().forEach(function (t) {
            t.stop();
          });
          finishRecording(new Blob(chunks, { type: recorder.mimeType }), url);
        });
        recorder.start();
        rec = {
          recorder: recorder,
          started: Date.now(),
          timer: setInterval(function () {
            if (rec && Date.now() - rec.started >= VOICE_MAX_MS) {
              stopRecording();
            }
            showVoice();
          }, 250),
        };
        showVoice();
      },
      function (err) {
        asking = false;
        note(
          err && err.name === "NotAllowedError"
            ? "Microphone access is blocked for this page."
            : "No microphone found."
        );
      }
    );
  }

  function stopRecording() {
    if (!rec) {
      return;
    }
    var r = rec;
    rec = null;
    clearInterval(r.timer);
    transcribing = true;
    showVoice();
    if (r.recorder.state !== "inactive") {
      r.recorder.stop();
    }
  }

  // A WAV file (PCM, mono, 16-bit) from samples in -1..1.
  function encodeWav(samples, rate) {
    var buf = new ArrayBuffer(44 + samples.length * 2);
    var v = new DataView(buf);
    function str(at, s) {
      for (var i = 0; i < s.length; i++) {
        v.setUint8(at + i, s.charCodeAt(i));
      }
    }
    str(0, "RIFF");
    v.setUint32(4, 36 + samples.length * 2, true);
    str(8, "WAVEfmt ");
    v.setUint32(16, 16, true);
    v.setUint16(20, 1, true);
    v.setUint16(22, 1, true);
    v.setUint32(24, rate, true);
    v.setUint32(28, rate * 2, true);
    v.setUint16(32, 2, true);
    v.setUint16(34, 16, true);
    str(36, "data");
    v.setUint32(40, samples.length * 2, true);
    for (var i = 0; i < samples.length; i++) {
      var x = Math.max(-1, Math.min(1, samples[i]));
      v.setInt16(44 + i * 2, x < 0 ? x * 0x8000 : x * 0x7fff, true);
    }
    return new Blob([buf], { type: "audio/wav" });
  }

  // The browser's own recording (webm/opus, mp4/aac, ...) as 16 kHz mono WAV.
  function toWav(data) {
    var Ctx = window.AudioContext || window.webkitAudioContext;
    var ctx = new Ctx();
    var decoded = ctx.decodeAudioData(data);
    decoded.then(
      function () {
        ctx.close();
      },
      function () {
        ctx.close();
      }
    );
    return decoded
      .then(function (buffer) {
        var frames = Math.max(1, Math.ceil(buffer.duration * VOICE_RATE));
        var off = new OfflineAudioContext(1, frames, VOICE_RATE);
        var src = off.createBufferSource();
        src.buffer = buffer;
        src.connect(off.destination);
        src.start();
        return off.startRendering();
      })
      .then(function (out) {
        return encodeWav(out.getChannelData(0), VOICE_RATE);
      });
  }

  // Put the text where the cursor is, with spaces around it as needed.
  function insertText(text) {
    var box = composer();
    if (!box) {
      return;
    }
    var start = box.selectionStart;
    var end = box.selectionEnd;
    var before = box.value.slice(0, start);
    var after = box.value.slice(end);
    var piece =
      (before && !/\s$/.test(before) ? " " : "") + text + (after && !/^\s/.test(after) ? " " : "");
    box.setRangeText(piece, start, end, "end");
    grow(box);
    box.focus();
  }

  function finishRecording(blob, url) {
    var failed = "Couldn't turn that into text.";
    blob
      .arrayBuffer()
      .then(toWav)
      .then(function (wav) {
        return fetch(url, {
          method: "POST",
          headers: { "Content-Type": "audio/wav" },
          body: wav,
          credentials: "same-origin",
        });
      })
      .then(function (res) {
        return res
          .json()
          .catch(function () {
            return {};
          })
          .then(function (body) {
            if (!res.ok || typeof body.text !== "string") {
              throw new Error(body.error || failed);
            }
            return body.text;
          });
      })
      .then(function (text) {
        if (text === "") {
          note("No speech heard.");
        } else {
          insertText(text);
        }
      })
      .catch(function (e) {
        // Our own messages are plain Errors; decoding or network failures get the short one.
        note(e && e.name === "Error" && e.message ? e.message : failed);
      })
      .then(function () {
        transcribing = false;
        showVoice();
      });
  }

  document.addEventListener("click", function (e) {
    var b = e.target.closest ? e.target.closest("#voice") : null;
    if (!b || transcribing || asking) {
      return;
    }
    if (rec) {
      stopRecording();
    } else {
      startRecording(b.dataset.url);
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

  // --- The model picker (a popover; the browser opens and closes it, Esc and clicks outside
  // included). Here: the provider rail filters the list, the search filters as you type,
  // favorites (starred, per browser in localStorage), Ctrl+1..9 pick the first models shown,
  // "Show more" for long lists, arrow keys move between models. A refused switch comes back
  // with the picker marked `data-error`; it opens again to say why.

  document.documentElement.classList.add("js");

  var FAVORITES = "melissa.favorite-models";
  var FIRST_SHOWN = 8;
  var pickerFilter = null;
  var pickerExpanded = false;
  var anchored = !!(window.CSS && CSS.supports && CSS.supports("position-area", "top"));

  function picker() {
    return document.getElementById("model-picker");
  }

  function favorites() {
    try {
      var list = JSON.parse(window.localStorage.getItem(FAVORITES) || "[]");
      return Array.isArray(list) ? list : [];
    } catch (e) {
      return [];
    }
  }

  function saveFavorites(list) {
    try {
      window.localStorage.setItem(FAVORITES, JSON.stringify(list));
    } catch (e) {
      // Storage unavailable: the star works until the page reloads.
    }
  }

  function each(list, fn) {
    Array.prototype.forEach.call(list, fn);
  }

  // The picks shown now, in order.
  function shownPicks(p) {
    var out = [];
    each(p.querySelectorAll(".picker-row"), function (row) {
      if (!row.hidden) {
        out.push(row.querySelector(".pick"));
      }
    });
    return out;
  }

  function applyPicker() {
    var p = picker();
    if (!p) {
      return;
    }
    var rail = p.querySelectorAll(".rail-item");
    if (!pickerFilter || !p.querySelector('.rail-item[data-filter="' + pickerFilter + '"]')) {
      var current = p.querySelector('.rail-item[data-filter]:not([data-filter="favorites"])[aria-pressed="true"]');
      var first = p.querySelector('.rail-item[data-filter]:not([data-filter="favorites"])');
      pickerFilter = (current || first) ? (current || first).getAttribute("data-filter") : null;
    }
    each(rail, function (b) {
      b.setAttribute("aria-pressed", b.getAttribute("data-filter") === pickerFilter ? "true" : "false");
    });
    var search = p.querySelector(".picker-search");
    var q = search ? search.value.trim().toLowerCase() : "";
    var favs = favorites();
    var matching = 0;
    var hiddenByLimit = 0;
    each(p.querySelectorAll(".picker-row"), function (row) {
      var value = row.getAttribute("data-value");
      var fav = favs.indexOf(value) >= 0;
      var star = row.querySelector(".fav");
      if (star) {
        star.setAttribute("aria-pressed", fav ? "true" : "false");
      }
      var show;
      if (q !== "") {
        // Searching looks through every provider.
        show = (row.getAttribute("data-name") || "").indexOf(q) >= 0;
      } else if (pickerFilter === "favorites") {
        show = fav;
      } else {
        show = row.getAttribute("data-backend") === pickerFilter;
      }
      if (show) {
        matching += 1;
        var isCurrent = row.querySelector('.pick[aria-current="true"]');
        if (q === "" && !pickerExpanded && matching > FIRST_SHOWN && !isCurrent) {
          show = false;
          hiddenByLimit += 1;
        }
      }
      row.hidden = !show;
    });
    each(p.querySelectorAll(".picker-group"), function (g) {
      g.hidden = !g.querySelector(".picker-row:not([hidden])");
    });
    var picks = shownPicks(p);
    each(p.querySelectorAll(".picker-row kbd"), function (k) {
      k.textContent = "";
    });
    picks.slice(0, 9).forEach(function (pick, i) {
      var k = pick.querySelector("kbd");
      if (k) {
        k.textContent = "Ctrl " + (i + 1);
      }
    });
    var noMatch = p.querySelector('[data-empty="search"]');
    var noFavs = p.querySelector('[data-empty="favorites"]');
    if (noMatch) {
      noMatch.hidden = !(q !== "" && picks.length === 0);
    }
    if (noFavs) {
      noFavs.hidden = !(q === "" && pickerFilter === "favorites" && picks.length === 0);
    }
    var more = p.querySelector(".picker-more");
    if (more) {
      more.hidden = hiddenByLimit === 0;
    }
  }

  // Above the chip, right-aligned with it, inside the window (only where CSS anchors are
  // missing; element.style is allowed by the CSP, unlike style attributes).
  function placePicker(p) {
    if (anchored) {
      return;
    }
    var chip = document.querySelector("#model-chip .model-chip");
    if (!chip) {
      return;
    }
    var r = chip.getBoundingClientRect();
    var width = Math.min(640, window.innerWidth - 32);
    var left = Math.max(16, Math.min(r.right - width, window.innerWidth - width - 16));
    p.classList.add("placed");
    p.style.left = left + "px";
    p.style.bottom = Math.max(16, window.innerHeight - r.top + 8) + "px";
  }

  document.addEventListener(
    "toggle",
    function (e) {
      var p = picker();
      if (e.target !== p) {
        return;
      }
      if (e.newState === "open") {
        var search = p.querySelector(".picker-search");
        if (search) {
          search.value = "";
        }
        pickerFilter = null;
        pickerExpanded = false;
        applyPicker();
        placePicker(p);
        var focus = search || p.querySelector('.pick[aria-current="true"]') || p.querySelector(".pick");
        if (focus) {
          focus.focus();
        }
      }
    },
    true
  );

  document.addEventListener("click", function (e) {
    var p = picker();
    if (!p || !e.target.closest || !p.contains(e.target)) {
      return;
    }
    var rail = e.target.closest(".rail-item");
    if (rail) {
      pickerFilter = rail.getAttribute("data-filter");
      pickerExpanded = false;
      var search = p.querySelector(".picker-search");
      if (search) {
        search.value = "";
      }
      applyPicker();
      return;
    }
    var star = e.target.closest(".fav");
    if (star) {
      var value = star.getAttribute("data-fav");
      var list = favorites();
      var i = list.indexOf(value);
      if (i >= 0) {
        list.splice(i, 1);
      } else {
        list.push(value);
      }
      saveFavorites(list);
      applyPicker();
      return;
    }
    if (e.target.closest(".picker-more")) {
      pickerExpanded = true;
      applyPicker();
    }
  });

  document.addEventListener("input", function (e) {
    if (e.target.classList && e.target.classList.contains("picker-search")) {
      applyPicker();
    }
  });

  document.addEventListener("keydown", function (e) {
    var p = picker();
    if (!p || !p.matches(":popover-open") || !p.contains(document.activeElement)) {
      return;
    }
    var picks = shownPicks(p);
    if (e.ctrlKey && !e.altKey && !e.metaKey && e.key >= "1" && e.key <= "9") {
      var pick = picks[Number(e.key) - 1];
      if (pick) {
        e.preventDefault();
        pick.click();
      }
      return;
    }
    var inSearch = document.activeElement.classList.contains("picker-search");
    if (e.key === "Enter" && inSearch) {
      // Enter in the search picks the first match instead of sending the form as is.
      e.preventDefault();
      if (picks[0]) {
        picks[0].click();
      }
      return;
    }
    if (e.key === "ArrowDown" || e.key === "ArrowUp") {
      var at = picks.indexOf(document.activeElement);
      var next;
      if (e.key === "ArrowDown") {
        next = inSearch ? picks[0] : picks[at + 1];
      } else {
        next = at > 0 ? picks[at - 1] : p.querySelector(".picker-search");
      }
      if (next) {
        e.preventDefault();
        next.focus();
      }
    }
  });

  // After a switch the picker comes back closed: focus returns to the chip, or the picker
  // opens again to show why the switch was refused.
  document.addEventListener("htmx:afterSwap", function (e) {
    var t = e.detail && e.detail.target;
    if (!t || t.id !== "model-picker") {
      return;
    }
    var p = picker();
    if (p && p.hasAttribute("data-error") && p.showPopover) {
      p.removeAttribute("data-error");
      p.showPopover();
      return;
    }
    // After the chip's own (out-of-band) swap, which replaces the focused button.
    setTimeout(function () {
      var chip = document.querySelector("#model-chip .model-chip");
      if (chip) {
        chip.focus();
      }
    }, 0);
  });

  // --- The details panel beside the chat (the agent, its apps, schedules and files). -------
  // Wide screens: a column; Details hides or shows it. Narrow screens: a sheet over the chat;
  // Details opens it, Close, Esc or a click outside shut it, focus moves in and back, and the
  // chat can't be reached while it is open (inert). Whether it is open is remembered per
  // browser and per width (localStorage; without it: open when wide, closed when narrow).
  // Without script the panel is simply always shown.

  var WIDE = window.matchMedia ? window.matchMedia("(min-width: 1200px)") : null;

  function isWide() {
    return !WIDE || WIDE.matches;
  }

  function storedOpen(wide) {
    try {
      var v = window.localStorage.getItem("melissa.details." + (wide ? "wide" : "narrow"));
      if (v === "1" || v === "0") {
        return v === "1";
      }
    } catch (e) {
      // Storage unavailable: the default.
    }
    return wide;
  }

  function storeOpen(wide, open) {
    try {
      window.localStorage.setItem("melissa.details." + (wide ? "wide" : "narrow"), open ? "1" : "0");
    } catch (e) {
      // Storage unavailable: nothing to remember.
    }
  }

  function detailsParts() {
    var panel = document.getElementById("details-panel");
    if (!panel) {
      return null;
    }
    return {
      panel: panel,
      toggle: document.querySelector("[data-details-toggle]"),
      backdrop: document.querySelector(".details-backdrop"),
      // What the sheet covers: made inert while it is open.
      covered: document.querySelectorAll(".shell > .rail, .chat-layout > .chat, .banner, .skip"),
    };
  }

  // Show the panel open or closed for the current width (`focus`: move focus as a user
  // action would).
  function showDetails(open, focus) {
    var p = detailsParts();
    if (!p) {
      return;
    }
    var wide = isWide();
    var sheet = open && !wide;
    if (wide) {
      p.panel.removeAttribute("data-open");
      if (open) {
        p.panel.removeAttribute("data-closed");
      } else {
        p.panel.setAttribute("data-closed", "");
      }
    } else {
      p.panel.removeAttribute("data-closed");
      if (open) {
        p.panel.setAttribute("data-open", "");
      } else {
        p.panel.removeAttribute("data-open");
      }
    }
    if (p.backdrop) {
      p.backdrop.hidden = !sheet;
    }
    Array.prototype.forEach.call(p.covered, function (el) {
      el.inert = sheet;
    });
    if (p.toggle) {
      p.toggle.setAttribute("aria-expanded", open ? "true" : "false");
    }
    if (focus) {
      if (open) {
        p.panel.focus();
      } else if (p.toggle) {
        p.toggle.focus();
      }
    }
  }

  function detailsOpen() {
    var p = detailsParts();
    if (!p) {
      return false;
    }
    return isWide() ? !p.panel.hasAttribute("data-closed") : p.panel.hasAttribute("data-open");
  }

  function setDetails(open, focus) {
    storeOpen(isWide(), open);
    showDetails(open, focus);
  }

  document.addEventListener("click", function (e) {
    var t = e.target;
    if (!(t instanceof Element)) {
      return;
    }
    if (t.closest("[data-details-toggle]")) {
      setDetails(!detailsOpen(), true);
      return;
    }
    if (t.closest("[data-details-close]")) {
      setDetails(false, true);
      return;
    }
    // "Reference in chat": put the file's reference in the message box (no page load).
    var ref = t.closest("a[data-insert]");
    if (ref && composer()) {
      e.preventDefault();
      if (!isWide()) {
        setDetails(false, false);
      }
      var more = ref.closest("details");
      if (more) {
        more.open = false;
      }
      insertText(ref.getAttribute("data-insert"));
    }
  });

  document.addEventListener("keydown", function (e) {
    if (e.key === "Escape" && !isWide() && detailsOpen()) {
      // Not while a popover or dialog above it handles its own Escape.
      if (document.querySelector(":popover-open, dialog[open]")) {
        return;
      }
      e.preventDefault();
      setDetails(false, true);
    }
  });

  function initDetails() {
    var p = detailsParts();
    if (!p) {
      return;
    }
    if (p.toggle) {
      p.toggle.hidden = false;
    }
    Array.prototype.forEach.call(p.panel.querySelectorAll(".details-close"), function (b) {
      b.hidden = false;
    });
    showDetails(storedOpen(isWide()), false);
    // Slide from now on (not while the page settles).
    window.requestAnimationFrame(function () {
      window.requestAnimationFrame(function () {
        p.panel.setAttribute("data-ready", "");
      });
    });
    if (WIDE && WIDE.addEventListener) {
      WIDE.addEventListener("change", function () {
        showDetails(storedOpen(isWide()), false);
      });
    }
  }

  document.addEventListener("DOMContentLoaded", function () {
    grow(composer());
    showVoice();
    initDetails();
    if (document.getElementById("feed")) {
      scrollToBottom();
    }
  });
})();
