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

  function each(list, fn) {
    Array.prototype.forEach.call(list, fn);
  }

  // Sign-in: every request that changes something carries the session's CSRF token (the
  // page's <meta name="csrf-token">). htmx and fetch send it as a header; a form posted
  // without htmx gets the hidden field if it has none (forms that came in with a live
  // update). Without script the server puts the field in every form itself.
  function csrfToken() {
    var meta = document.querySelector('meta[name="csrf-token"]');
    return meta ? meta.getAttribute("content") : "";
  }

  document.addEventListener("htmx:configRequest", function (e) {
    var token = csrfToken();
    if (token) {
      e.detail.headers["X-CSRF-Token"] = token;
    }
  });

  document.addEventListener(
    "submit",
    function (e) {
      var form = e.target;
      var token = csrfToken();
      if (!token || !form || (form.getAttribute("method") || "").toLowerCase() !== "post") {
        return;
      }
      if (!form.querySelector('input[name="_csrf"]')) {
        var field = document.createElement("input");
        field.type = "hidden";
        field.name = "_csrf";
        field.value = token;
        form.insertBefore(field, form.firstChild);
      }
    },
    true
  );

  // fetch() with the token; a sensitive action that needs the password again goes to the
  // password page and comes back.
  function postSame(url, init) {
    init.headers = init.headers || {};
    init.headers["X-CSRF-Token"] = csrfToken();
    init.credentials = "same-origin";
    return fetch(url, init).then(function (res) {
      var again = res.headers.get("X-Melissa-Reauth");
      if (res.status === 401 && again && again.charAt(0) === "/") {
        window.location.assign(again);
      }
      return res;
    });
  }

  // Remembered choices, per browser (or per tab with `session`). Storage can be blocked
  // (private windows): then nothing is remembered and reading gives null.
  function stored(key, session) {
    try {
      return (session ? window.sessionStorage : window.localStorage).getItem(key);
    } catch (e) {
      return null;
    }
  }

  function store(key, value, session) {
    if (!session) {
      mirror(key, value);
    }
    try {
      (session ? window.sessionStorage : window.localStorage).setItem(key, value);
    } catch (e) {
      // Not remembered.
    }
  }

  // Choices that change how the page looks are also kept in a cookie, so the server sends the
  // page that way and nothing moves after the first paint (web `looks.rs`, 2026-11-22): key in
  // storage -> [cookie, stored value -> cookie value (else the cookie goes)].
  var LOOKS = {
    "melissa.dismissed.prealpha": ["melissa_banner", { 1: "dismissed" }],
    "melissa.sidebar": ["melissa_sidebar", { collapsed: "collapsed" }],
    "melissa.details.wide": ["melissa_details", { 0: "closed" }],
  };

  function mirror(key, value) {
    var look = LOOKS[key];
    if (!look) {
      return;
    }
    var v = look[1][value];
    var secure = window.location.protocol === "https:" ? "; Secure" : "";
    document.cookie =
      look[0] + "=" + (v || "") + "; Path=/; SameSite=Lax" + (v ? "; Max-Age=31536000" : "; Max-Age=0") + secure;
  }

  // Choices made before the cookies existed: copy them over once per page (cheap).
  Object.keys(LOOKS).forEach(function (key) {
    var v = stored(key);
    if (v !== null) {
      mirror(key, v);
    }
  });

  // What scrolls the chat: the nearest scrolling box around the feed (the main column, or the
  // chat column beside the details panel), or the page on phones.
  function scroller() {
    var el = document.getElementById("feed");
    while (el && el !== document.body) {
      var o = window.getComputedStyle(el).overflowY;
      if (o === "auto" || o === "scroll") {
        return el;
      }
      el = el.parentElement;
    }
    return document.scrollingElement || document.documentElement;
  }

  function nearBottom() {
    var s = scroller();
    return s.scrollTop + s.clientHeight >= s.scrollHeight - 200;
  }

  function scrollToBottom() {
    var s = scroller();
    s.scrollTop = s.scrollHeight;
  }

  function composer() {
    return document.getElementById("message");
  }

  // Enter sends, Shift+Enter starts a new line (not while an input method is composing).
  // While the agent works (the Stop button shows) Enter queues the message and a quick
  // second Enter steers it into the running turn instead, or the other way round with
  // Settings → Developer → "Enter while working: Steers" (the form's `data-enter`). The
  // first Enter waits briefly for a second one, so the message is sent once either way.
  var DOUBLE_ENTER_MS = 400;
  var enterTimer = null;

  function working() {
    return !!document.querySelector("#composer-status .working");
  }

  function sendAs(form, mode) {
    var field = form.querySelector("input[name=mode]");
    if (field) {
      field.value = mode;
    }
    form.requestSubmit();
  }

  document.addEventListener("keydown", function (e) {
    var box = composer();
    if (e.target !== box || e.key !== "Enter" || e.shiftKey || e.isComposing) {
      return;
    }
    e.preventDefault();
    var form = box.form;
    if (!form) {
      return;
    }
    if (enterTimer !== null) {
      // The second Enter: the other way.
      clearTimeout(enterTimer);
      enterTimer = null;
      sendAs(form, form.dataset.enter === "steer" ? "queue" : "steer");
      return;
    }
    if (box.value.trim() === "" && kept.length === 0) {
      return;
    }
    if (!working()) {
      sendAs(form, "");
      return;
    }
    enterTimer = setTimeout(function () {
      enterTimer = null;
      sendAs(form, form.dataset.enter === "steer" ? "steer" : "queue");
    }, DOUBLE_ENTER_MS);
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
      draftSoon();
    }
  });

  // --- Drafts: each chat keeps its unsent text, per browser. ---------------------------------
  // The composer form's `data-draft` names the chat. The text is saved shortly after typing and
  // when the page goes away (switching chats is a page load), put back when the chat opens
  // again, and cleared once the message is sent. Files are not kept. Without storage the box
  // simply starts empty, as before.

  var draftTimer = null;

  function draftKey() {
    var box = composer();
    var key = box && box.form ? box.form.getAttribute("data-draft") : null;
    return key ? "melissa.draft." + key : null;
  }

  function forget(key) {
    try {
      if (key) {
        window.localStorage.removeItem(key);
      }
    } catch (e) {
      // Nothing stored.
    }
  }

  function saveDraft() {
    clearTimeout(draftTimer);
    draftTimer = null;
    var key = draftKey();
    if (!key) {
      return;
    }
    var text = composer().value;
    if (text.trim() === "") {
      forget(key);
    } else {
      store(key, text);
    }
  }

  function draftSoon() {
    clearTimeout(draftTimer);
    draftTimer = setTimeout(saveDraft, 400);
  }

  // On page load: an empty box gets its draft back. Text the server put in the box (a message
  // it refused, to retry) wins and becomes the draft.
  function restoreDraft() {
    var key = draftKey();
    var box = composer();
    if (!key) {
      return;
    }
    if (box.value !== "") {
      saveDraft();
      return;
    }
    var text = stored(key);
    if (text) {
      box.value = text;
      box.setSelectionRange(text.length, text.length);
      grow(box);
    }
  }

  // Sending: forget the draft now, so leaving the chat right away doesn't bring the sent text
  // back. A refused message comes back in the box and is saved again (after the swap).
  document.addEventListener("submit", function (e) {
    // (Stop is a submit button with its own `formaction`: that keeps the draft.)
    var other = e.submitter && e.submitter.hasAttribute("formaction");
    if (e.target && e.target.id === "composer" && !other) {
      clearTimeout(draftTimer);
      draftTimer = null;
      forget(draftKey());
    }
  });

  window.addEventListener("pagehide", function () {
    if (draftTimer !== null) {
      saveDraft();
    }
  });

  document.addEventListener("visibilitychange", function () {
    if (document.visibilityState === "hidden" && draftTimer !== null) {
      saveDraft();
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
    each(files, function (f) {
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
    // Fields that act on change: the details panel's upload icon and its "Keep for" choice.
    var t = e.target;
    if (t && t.hasAttribute && t.hasAttribute("data-autosubmit") && (!t.files || t.files.length > 0)) {
      if (t.form && t.form.requestSubmit) {
        t.form.requestSubmit();
      }
      return;
    }
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
      // Sent: the box is empty and the draft goes. Refused: the box has the text back.
      saveDraft();
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
    saveDraft();
  }

  function finishRecording(blob, url) {
    var failed = "Couldn't turn that into text.";
    blob
      .arrayBuffer()
      .then(toWav)
      .then(function (wav) {
        return postSame(url, {
          method: "POST",
          headers: { "Content-Type": "audio/wav" },
          body: wav,
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

  // Copy on a setup password card: the value comes from the server (POST, same origin,
  // no-store) straight into the clipboard; it never goes into the page. Safari wants the
  // clipboard write started inside the click, so a ClipboardItem takes the pending fetch.
  document.addEventListener("click", function (e) {
    var button = e.target.closest ? e.target.closest(".copy-secret") : null;
    if (!button || !navigator.clipboard) {
      return;
    }
    var card = button.closest(".setup-password");
    var text = postSame(button.dataset.copyUrl, {
      method: "POST",
      headers: { Accept: "text/plain", "Content-Type": "application/x-www-form-urlencoded" },
      body: "purpose=copy",
      cache: "no-store",
    }).then(function (r) {
      if (!r.ok) {
        throw new Error(String(r.status));
      }
      return r.text();
    });
    var write;
    if (window.ClipboardItem && navigator.clipboard.write) {
      var blob = text.then(function (t) {
        return new Blob([t], { type: "text/plain" });
      });
      write = navigator.clipboard.write([new ClipboardItem({ "text/plain": blob })]);
    } else {
      write = text.then(function (t) {
        return navigator.clipboard.writeText(t);
      });
    }
    write.then(
      function () {
        button.textContent = "Copied";
        setTimeout(function () {
          button.textContent = "Copy";
        }, 1500);
      },
      function () {
        // Dismissed or gone meanwhile, or no answer: the card shows why.
        if (card && window.htmx) {
          htmx.trigger(card, "setup-refresh");
        }
      }
    );
  });

  // A setup password card answers "gone", "dismissed" and "couldn't reach" with a card in
  // that state; let htmx swap it in although the status is an error.
  document.addEventListener("htmx:beforeSwap", function (e) {
    var status = e.detail.xhr ? e.detail.xhr.status : 0;
    var elt = e.detail.elt;
    if ((status === 404 || status === 410 || status === 502) && elt && elt.closest &&
        elt.closest(".setup-password")) {
      e.detail.shouldSwap = true;
      e.detail.isError = false;
    }
  });

  // --- Sign-in pages: "Show password" switches its field between hidden and plain text. ----
  // Without script the switch stays hidden (CSS `.js .show-password`) and the field hidden.

  document.addEventListener("change", function (e) {
    var box = e.target;
    if (!box.matches || !box.matches("input[data-show-password]")) return;
    var wrap = box.closest(".field-pw");
    var field = wrap && wrap.querySelector("label.field input");
    if (field) field.type = box.checked ? "text" : "password";
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
      var list = JSON.parse(stored(FAVORITES) || "[]");
      return Array.isArray(list) ? list : [];
    } catch (e) {
      return [];
    }
  }

  // Without storage the star works until the page reloads.
  function saveFavorites(list) {
    store(FAVORITES, JSON.stringify(list));
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
    var v = stored("melissa.details." + (wide ? "wide" : "narrow"));
    return v === "1" || v === "0" ? v === "1" : wide;
  }

  function storeOpen(wide, open) {
    store("melissa.details." + (wide ? "wide" : "narrow"), open ? "1" : "0");
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
      covered: document.querySelectorAll(".shell > .sidebar, .chat-layout > .chat, .banner, .skip"),
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
    // The phone's agent list hides the chat and its sheet: then nothing is covered.
    var main = p.panel.closest("main");
    var sheet = open && !wide && !!main && main.getClientRects().length > 0;
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
    each(p.covered, function (el) {
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
    // Review: to the permission card in the chat; on narrow screens the sheet steps aside.
    if (t.closest("[data-review]") && !isWide()) {
      setDetails(false, false);
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
    showDetails(storedOpen(isWide()), false);
    // Slide from now on (not while the page settles).
    window.requestAnimationFrame(function () {
      window.requestAnimationFrame(function () {
        p.panel.setAttribute("data-ready", "");
      });
    });
  }

  // Bound once (initDetails runs again after each in-page navigation); without a panel on the
  // page showDetails does nothing.
  if (WIDE && WIDE.addEventListener) {
    WIDE.addEventListener("change", function () {
      showDetails(storedOpen(isWide()), false);
    });
  }

  // --- The sidebar (2026-11-20, owner design): agents grouped by state. --------------------
  // Here: the search filters the agents as you type; wide screens collapse it (remembered per
  // browser) and the chat header's button shows it again. Working keeps the open state
  // the user gave them across live updates (per tab), and agents glide to their new place
  // (motion section below). On phones it is the agent list (CSS;
  // `components::mobile`), so collapsing doesn't apply there. Without script it is a plain
  // column.

  var PHONE = window.matchMedia ? window.matchMedia("(max-width: 760px)") : null;

  function isPhone() {
    return !!(PHONE && PHONE.matches);
  }

  function sideCollapsed() {
    return document.documentElement.getAttribute("data-sidebar") === "collapsed";
  }

  function showSideButtons() {
    var collapsed = sideCollapsed();
    each(document.querySelectorAll("[data-sidebar-expand]"), function (b) {
      b.hidden = !collapsed;
    });
  }

  function setCollapsed(collapsed) {
    if (collapsed) {
      document.documentElement.setAttribute("data-sidebar", "collapsed");
    } else {
      document.documentElement.removeAttribute("data-sidebar");
    }
    store("melissa.sidebar", collapsed ? "collapsed" : "open");
    showSideButtons();
    var to = collapsed
      ? document.querySelector(".chat-head [data-sidebar-expand]")
      : document.getElementById("agent-search");
    if (to) {
      to.focus();
    }
  }

  document.addEventListener("click", function (e) {
    var t = e.target;
    if (!(t instanceof Element)) {
      return;
    }
    var collapse = t.closest("[data-sidebar-collapse]");
    if (collapse) {
      var menu = collapse.closest("[popover]");
      if (menu && menu.hidePopover && menu.matches(":popover-open")) {
        menu.hidePopover();
      }
      setCollapsed(true);
      return;
    }
    if (t.closest("[data-sidebar-expand]")) {
      setCollapsed(false);
    }
  });

  // The search: rows whose name has the text; groups without any are hidden; closed groups
  // open while searching (and close again after).
  function filterAgents() {
    var input = document.querySelector("[data-agent-search]");
    var groups = document.getElementById("agent-groups");
    if (!input || !groups) {
      return;
    }
    var q = input.value.trim().toLowerCase();
    var any = false;
    each(groups.querySelectorAll(".agent-row"), function (row) {
      row.hidden = q !== "" && (row.getAttribute("data-name") || "").indexOf(q) < 0;
      any = any || !row.hidden;
    });
    each(groups.querySelectorAll(".agent-group"), function (g) {
      var shown = !!g.querySelector(".agent-row:not([hidden])");
      g.hidden = q !== "" && !shown;
      if (g.tagName === "DETAILS") {
        if (q !== "" && !g.hasAttribute("data-was")) {
          g.setAttribute("data-was", g.open ? "1" : "0");
          g.open = true;
        } else if (q === "" && g.hasAttribute("data-was")) {
          g.open = g.getAttribute("data-was") === "1";
          g.removeAttribute("data-was");
        }
      }
    });
    var none = document.querySelector("[data-search-empty]");
    if (none) {
      none.hidden = q === "" || any;
    }
  }

  document.addEventListener("input", function (e) {
    if (e.target instanceof Element && e.target.matches("[data-agent-search]")) {
      filterAgents();
    }
  });

  // Working: what the user opened or closed stays so across live updates.
  function groupKey(g) {
    return "melissa.group." + g.getAttribute("data-group");
  }

  document.addEventListener(
    "toggle",
    function (e) {
      var g = e.target;
      if (!(g instanceof Element) || !g.matches("details.agent-group") || g.hasAttribute("data-was")) {
        return;
      }
      store(groupKey(g), g.open ? "1" : "0", true);
    },
    true
  );

  function restoreGroups() {
    each(document.querySelectorAll("details.agent-group[data-group]"), function (g) {
      var v = stored(groupKey(g), true);
      if (v === "1" || v === "0") {
        g.open = v === "1";
      }
    });
  }

  document.addEventListener("htmx:oobAfterSwap", function (e) {
    var t = e.detail && e.detail.target;
    if (t && t.id === "agent-groups") {
      restoreGroups();
      filterAgents();
      moveRows();
    }
  });

  // --- Motion (2026-11-21, owner): what arrives after the page loaded fades in, agents move
  // to their new place in the sidebar. The looks are in melissa.css ([data-arrive], the
  // keyframes); here only what CSS can't tell: what is new. Nothing runs with reduced motion,
  // and nothing waits for an animation (they are on transform and opacity only).
  var STILL = window.matchMedia ? window.matchMedia("(prefers-reduced-motion: reduce)") : null;

  function still() {
    return !!(STILL && STILL.matches);
  }

  // Fade `el` in (CSS: [data-arrive]); the mark goes when the animation ends.
  function arrive(el) {
    if (still() || !(el instanceof Element)) {
      return;
    }
    el.setAttribute("data-arrive", "");
  }

  function settled(e) {
    if (e.target instanceof Element && e.target.hasAttribute("data-arrive")) {
      e.target.removeAttribute("data-arrive");
    }
  }

  document.addEventListener("animationend", settled);
  document.addEventListener("animationcancel", settled);

  // In a form's answer only a notice (saved, failed) fades in: the rest of the answer is the
  // same section shown again, and fading it reads as a jump (owner, 2026-11-22). Panels and
  // Settings sections that load are still too. Not: live updates, polled regions and results
  // as you type (they come often).
  var NOTICES = ".notice, .sheet-msg";

  function swapStyleOf(el) {
    var s = el && el.closest ? el.closest("[hx-swap]") : null;
    return s ? (s.getAttribute("hx-swap") || "").split(" ")[0] : "innerHTML";
  }

  document.addEventListener("htmx:afterSwap", function (e) {
    var d = e.detail || {};
    var t = e.target;
    // Only answers to a request (live updates over the stream have no xhr).
    var from = d.xhr && d.requestConfig ? d.requestConfig.elt || d.elt : null;
    if (!(t instanceof Element) || !(from instanceof Element) || still() || t.id === "main") {
      return;
    }
    var trigger = from.getAttribute("hx-trigger") || "";
    if (/every|input|keyup/.test(trigger) || swapStyleOf(from) === "none") {
      return;
    }
    // A panel, a Settings section or a page that loaded: shown, not new.
    if (t.id === "overlay-body" || t.id === "main" || t.matches(".settings-main")) {
      return;
    }
    // innerHTML: the target stays and its children are new; outerHTML: the target is new.
    var fresh = t === d.target ? t.children : [t];
    each(fresh, function (el) {
      each(el.matches(NOTICES) ? [el] : el.querySelectorAll(NOTICES), arrive);
    });
  });

  // Live replacements (out of band): note what was there, so only what is new fades in and
  // the sidebar's rows can move from where they were.
  var sideBefore = null;
  var queueBefore = null;

  // A queued message by its Remove form (each has its own).
  function queueKey(msg) {
    var forms = msg.querySelectorAll(".queue-actions form[action]");
    return forms.length ? forms[forms.length - 1].getAttribute("action") : null;
  }

  function queueKeys(box) {
    var keys = {};
    each(box.querySelectorAll(".msg-queued"), function (m) {
      var k = queueKey(m);
      if (k) {
        keys[k] = true;
      }
    });
    return keys;
  }

  // Where each shown agent row is now (rows in a closed group or hidden by the search have
  // no place and are left alone).
  function rowPlaces(groups) {
    var rows = {};
    each(groups.querySelectorAll(".agent-row[data-agent]"), function (row) {
      if (row.getClientRects().length) {
        rows[row.getAttribute("data-agent")] = { el: row, rect: row.getBoundingClientRect() };
      }
    });
    return rows;
  }

  document.addEventListener("htmx:oobBeforeSwap", function (e) {
    var t = e.detail && e.detail.target;
    if (!t || still()) {
      return;
    }
    if (t.id === "agent-groups") {
      sideBefore = rowPlaces(t);
    } else if (t.id === "chat-queue") {
      queueBefore = queueKeys(t);
    }
  });

  document.addEventListener("htmx:oobAfterSwap", function (e) {
    var t = e.detail && e.detail.target;
    if (t && t.id === "chat-queue" && queueBefore) {
      var before = queueBefore;
      queueBefore = null;
      var box = document.getElementById("chat-queue");
      each(box ? box.querySelectorAll(".msg-queued") : [], function (m) {
        if (!before[queueKey(m)]) {
          arrive(m);
        }
      });
    }
  });

  // FLIP: each row that changed place starts where it was and slides to where it is now; a
  // new agent fades in; one that left fades out where it was (a copy without its menu).
  function moveRows() {
    var before = sideBefore;
    sideBefore = null;
    var groups = document.getElementById("agent-groups");
    if (!before || !groups || still()) {
      return;
    }
    var now = rowPlaces(groups);
    var here = {};
    each(groups.querySelectorAll(".agent-row[data-agent]"), function (row) {
      here[row.getAttribute("data-agent")] = true;
    });
    Object.keys(now).forEach(function (key) {
      var row = now[key].el;
      var was = before[key];
      if (!was) {
        arrive(row);
        return;
      }
      var dx = was.rect.left - now[key].rect.left;
      var dy = was.rect.top - now[key].rect.top;
      if ((Math.abs(dx) >= 1 || Math.abs(dy) >= 1) && row.animate) {
        row.animate(
          [{ transform: "translate(" + dx + "px, " + dy + "px)" }, { transform: "none" }],
          { duration: 200, easing: "ease-out" }
        );
      }
    });
    var box = groups.getBoundingClientRect();
    Object.keys(before).forEach(function (key) {
      if (!here[key] && before[key].el.animate) {
        leave(groups, before[key], box);
      }
    });
  }

  function leave(groups, was, box) {
    var ghost = was.el.cloneNode(true);
    each(ghost.querySelectorAll("[popover], [popovertarget]"), function (el) {
      el.remove();
    });
    each([ghost].concat(Array.prototype.slice.call(ghost.querySelectorAll("[id]"))), function (el) {
      el.removeAttribute("id");
    });
    ghost.removeAttribute("data-agent");
    ghost.removeAttribute("data-name");
    ghost.classList.add("leaving");
    ghost.setAttribute("aria-hidden", "true");
    ghost.inert = true;
    ghost.style.left = was.rect.left - box.left + "px";
    ghost.style.top = was.rect.top - box.top + "px";
    ghost.style.width = was.rect.width + "px";
    groups.appendChild(ghost);
    var a = ghost.animate([{ opacity: 1 }, { opacity: 0 }], { duration: 160, easing: "ease-out" });
    a.onfinish = a.oncancel = function () {
      ghost.remove();
    };
  }

  function initSidebar() {
    if (!document.getElementById("sidebar")) {
      return;
    }
    if (stored("melissa.sidebar") === "collapsed") {
      document.documentElement.setAttribute("data-sidebar", "collapsed");
    }
    showSideButtons();
    restoreGroups();
    filterAgents();
  }

  // --- Menus ("⋯", the device menu): native popovers; placed next to the button that opened
  // them, inside the window (without script the browser centres them).
  var menuInvoker = null;

  document.addEventListener(
    "click",
    function (e) {
      var t = e.target;
      if (t instanceof Element) {
        var b = t.closest("[popovertarget]");
        if (b) {
          menuInvoker = b;
        }
      }
    },
    true
  );

  function placeMenu(menu) {
    if (isPhone() && menu.classList.contains("agent-menu")) {
      // On phones an agent's menu is an action sheet at the bottom (CSS).
      menu.style.left = "";
      menu.style.top = "";
      return;
    }
    var b = menuInvoker && menuInvoker.getAttribute("popovertarget") === menu.id
      ? menuInvoker
      : document.querySelector('[popovertarget="' + CSS.escape(menu.id) + '"]');
    if (!b) {
      return;
    }
    var r = b.getBoundingClientRect();
    var w = menu.offsetWidth;
    var h = menu.offsetHeight;
    var left = Math.max(8, Math.min(r.right - w, window.innerWidth - w - 8));
    var top = r.bottom + 4;
    if (top + h > window.innerHeight - 8) {
      top = Math.max(8, r.top - h - 4);
    }
    menu.style.left = left + "px";
    menu.style.top = top + "px";
  }

  document.addEventListener(
    "toggle",
    function (e) {
      var m = e.target;
      if (m instanceof Element && m.matches(".menu[popover]") && e.newState === "open") {
        placeMenu(m);
        var first = m.querySelector("a, button, summary");
        if (first) {
          first.focus();
        }
      }
    },
    true
  );

  // --- Older messages: the top of the feed loads them as it scrolls into view, keeping what
  // the user looks at in place.
  var olderSeen = null;

  function loadOlder(marker) {
    var url = marker.getAttribute("data-older");
    if (!url || !window.htmx || marker.hasAttribute("data-loading")) {
      return;
    }
    marker.setAttribute("data-loading", "");
    var s = scroller();
    var fromBottom = s.scrollHeight - s.scrollTop;
    window.htmx.ajax("GET", url, { target: marker, swap: "outerHTML" }).then(function () {
      s.scrollTop = s.scrollHeight - fromBottom;
      watchOlder();
    });
  }

  function watchOlder() {
    if (olderSeen) {
      olderSeen.disconnect();
      olderSeen = null;
    }
    var marker = document.querySelector("#feed .feed-older");
    if (!marker || !window.IntersectionObserver) {
      return;
    }
    olderSeen = new IntersectionObserver(function (entries) {
      entries.forEach(function (en) {
        if (en.isIntersecting) {
          olderSeen.disconnect();
          loadOlder(en.target);
        }
      });
    });
    olderSeen.observe(marker);
  }

  // Panels opened at a part of them (`/settings/agents/<id>#agent-icon`): show that part.
  function showPanelPart(url) {
    var hash = (url || "").split("#")[1];
    if (!hash) {
      return;
    }
    var el = document.getElementById(hash);
    if (el) {
      el.scrollIntoView({ block: "start" });
      var field = el.querySelector("input:not([type=hidden]), select, textarea, summary");
      if (field) {
        field.focus();
      }
    }
  }

  document.addEventListener("click", function (e) {
    var a = e.target instanceof Element ? e.target.closest(".feed-older a") : null;
    if (a && window.htmx) {
      e.preventDefault();
      loadOlder(a.closest(".feed-older"));
    }
  });

  // Dismissable notices (the pre-alpha banner): the close button hides one for good in this
  // browser. Storage can be blocked (private windows); then it only hides for this page. The
  // cookie (LOOKS) has the server leave it out of later pages; the removal here is for pages
  // from before it was set.
  function initDismiss() {
    var buttons = document.querySelectorAll("[data-dismiss]");
    each(buttons, function (b) {
      var id = b.getAttribute("data-dismiss");
      var el = document.getElementById(id);
      if (!el) {
        return;
      }
      var key = "melissa.dismissed." + id;
      if (stored(key) === "1") {
        el.remove();
        return;
      }
      b.addEventListener("click", function () {
        store(key, "1");
        el.remove();
      });
    });
  }


  // Panels (Settings, Apps, Usage and resources) open in a modal over the current page. A
  // link to one loads it with htmx into #overlay-body (the server answers with the panel
  // alone) and pushes its URL, so reload and Back work; closing goes back to where the page
  // was. A page that is a panel (a deep link, a reload) moves its panel into the modal; closing
  // it then goes to the chat. Without script every panel is a plain page.
  var PANEL_PATHS = /^\/(settings(\/|$|\?)|tasks\/\d|cluster($|\?)|usage($|\?))/;

  function overlayParts() {
    var dialog = document.getElementById("overlay");
    return dialog && dialog.showModal ? { dialog: dialog, body: document.getElementById("overlay-body") } : null;
  }

  var panelState = { depth: 0, deep: false, opener: null, title: document.title, loaded: null };

  function panelPath(a) {
    if (!a || a.target || a.hasAttribute("download") || a.hasAttribute("hx-get")) {
      return null;
    }
    var href = a.getAttribute("href");
    if (!href || href.charAt(0) !== "/" || href.charAt(1) === "/") {
      return null;
    }
    return PANEL_PATHS.test(href) ? href : null;
  }

  function showPanel(o) {
    var panel = o.body.querySelector("[data-panel]");
    var title = panel ? panel.getAttribute("data-title") : "Panel";
    o.dialog.setAttribute("aria-label", title);
    document.title = title + " · Melissa";
    if (!o.dialog.open) {
      panelState.opener = document.activeElement;
      o.dialog.showModal();
    }
    initSettingsSearch(o.body);
    // Focus the section list's current item (Settings) or the panel's first control.
    var focus = o.body.querySelector(".settings-nav [aria-current='page']") || o.body.querySelector("[data-panel-close]");
    if (focus) {
      focus.focus();
    }
  }

  // Pages shown inside the Settings panel (a section, or a page below one).
  var SETTINGS_PATHS = /^\/(settings(\/|$|\?)|tasks\/\d)/;

  // Record where the panel is now in the history (`push`: a new entry).
  function notePanel(url, push) {
    var at = panelState.loaded || url;
    if (push) {
      panelState.depth += 1;
      window.history.pushState({ panel: at, depth: panelState.depth }, "", at);
    } else {
      window.history.replaceState({ panel: at, depth: panelState.depth }, "", at);
    }
  }

  // Settings is open and `url` is another of its pages: swap in only the section (breadcrumb
  // and content), so the section list, its scroll and the search stay as they are and nothing
  // fades (owner, 2026-11-22). Focus stays on the list entry that was clicked; if it was in
  // the old section, it goes to the new one.
  function openSection(o, url, push) {
    var main = document.getElementById("settings-main");
    var focusWasIn = main.contains(document.activeElement);
    panelState.loaded = null;
    window.htmx
      .ajax("GET", url, { target: "#settings-main", swap: "outerHTML", select: "#settings-main" })
      .then(function () {
        var now = document.getElementById("settings-main");
        if (!now || now === main) {
          // Not a Settings page after all (or it failed): load the whole panel instead.
          openPanel(url, push, true);
          return;
        }
        notePanel(url, push);
        var section = now.getAttribute("data-section");
        each(o.body.querySelectorAll(".settings-nav a[href]"), function (a) {
          if (a.getAttribute("href") === section) {
            a.setAttribute("aria-current", "page");
          } else {
            a.removeAttribute("aria-current");
          }
        });
        var input = o.body.querySelector("[data-settings-search] input");
        if (input && input.value) {
          filterSettings(o.body, input.value);
        }
        if (focusWasIn || !o.body.contains(document.activeElement)) {
          now.focus({ preventScroll: true });
        }
        showPanelPart(url);
      });
  }

  // An answer for the Settings section that has no section in it is not swapped in.
  document.addEventListener("htmx:beforeSwap", function (e) {
    var d = e.detail || {};
    var text = d.xhr ? d.xhr.responseText || "" : "";
    if (d.target && d.target.id === "settings-main" && text.indexOf('id="settings-main"') < 0) {
      d.shouldSwap = false;
    }
  });

  // Load `url` into the modal; `push`: a new history entry (else the current one is replaced).
  // `whole`: the whole panel even when only a Settings section would do.
  function openPanel(url, push, whole) {
    var o = overlayParts();
    if (!o || !window.htmx) {
      window.location.assign(url);
      return;
    }
    var inSettings = o.dialog.open && document.getElementById("settings-main");
    if (!whole && inSettings && SETTINGS_PATHS.test(url)) {
      openSection(o, url, push);
      return;
    }
    var before = o.body.firstElementChild;
    panelState.loaded = null;
    window.htmx
      .ajax("GET", url, { target: "#overlay-body", swap: "innerHTML" })
      .then(function () {
        var now = o.body.firstElementChild;
        if (!now || now === before || !now.hasAttribute("data-panel")) {
          // Nothing came back as a panel (an error, a page that isn't one): open it as a page.
          window.location.assign(url);
          return;
        }
        // Where it ended up (`/settings` redirects to the first section).
        notePanel(url, push);
        showPanel(o);
        showPanelPart(url);
      });
  }

  function closePanel() {
    var o = overlayParts();
    if (o && o.dialog.open) {
      o.dialog.close();
    }
  }

  function initPanel() {
    var o = overlayParts();
    if (!o) {
      return;
    }
    // Closing goes back to the page under the panel; a link in the panel to a page
    // (pageState.next) opens that page once the panel's history entries are gone.
    o.dialog.addEventListener("close", function () {
      document.title = panelState.title;
      o.body.replaceChildren();
      var back = panelState.depth;
      panelState.depth = 0;
      var next = pageState.next;
      if (panelState.deep) {
        // The panel was the page (a deep link): its page is the chat, or the link's page.
        panelState.deep = false;
        pageState.next = null;
        openPage(next ? next.url : "/", true, next ? next.hash : "");
        return;
      }
      if (!next && panelState.opener && panelState.opener.focus) {
        panelState.opener.focus();
      }
      if (back > 0) {
        window.history.go(-back);
      } else if (next) {
        pageState.next = null;
        openPage(next.url, next.url !== pageState.shown, next.hash);
      } else if (window.history.state && window.history.state.panel && pageState.shown) {
        // Back into a panel and closed: the address is the page's again.
        window.history.replaceState({ page: pageState.shown }, "", pageState.shown);
      }
    });
    // A click on the backdrop (the dialog itself, outside its content) closes it.
    o.dialog.addEventListener("click", function (e) {
      if (e.target === o.dialog) {
        closePanel();
      }
    });
    window.addEventListener("popstate", function (e) {
      var st = e.state;
      if (st && st.panel) {
        panelState.depth = st.depth || 0;
        openPanel(st.panel, false);
      } else if (o.dialog.open && !panelState.deep) {
        panelState.depth = 0;
        o.dialog.close();
      }
    });
    // This page is a panel: show it as the modal (closing goes to the chat).
    var main = document.getElementById("main");
    var own = main && main.querySelector(":scope > [data-panel]");
    if (own) {
      panelState.deep = true;
      pageState.shown = null;
      o.body.appendChild(own);
      window.history.replaceState({ panel: window.location.pathname + window.location.search, depth: 0 }, "");
      showPanel(o);
      showPanelPart(window.location.href);
    }
  }

  document.addEventListener("click", function (e) {
    if (e.defaultPrevented || e.button !== 0 || e.metaKey || e.ctrlKey || e.shiftKey || e.altKey) {
      return;
    }
    var t = e.target;
    if (!(t instanceof Element)) {
      return;
    }
    if (t.closest("[data-panel-close]") && overlayParts() && overlayParts().dialog.open) {
      e.preventDefault();
      closePanel();
      return;
    }
    var url = panelPath(t.closest("a[href]"));
    if (url && overlayParts()) {
      e.preventDefault();
      openPanel(url, true);
    }
  });

  document.addEventListener("htmx:afterRequest", function (e) {
    var d = e.detail;
    var into = d && d.target ? d.target.id : "";
    if ((into === "overlay-body" || into === "settings-main") && d.xhr && d.xhr.responseURL) {
      try {
        var u = new URL(d.xhr.responseURL);
        if (u.origin === window.location.origin) {
          panelState.loaded = u.pathname + u.search;
        }
      } catch (err) {
        // Keep the requested URL.
      }
    }
  });

  // --- Pages without full loads (2026-11-22, owner: "we should not need full page loads in
  // general"). A link to a page with the sidebar (a chat, earlier chats, everything waiting)
  // swaps only the main column: htmx asks for the page into #main, the server answers with the
  // main column's content, its title and the sidebar's agent groups out of band (the selected
  // agent, the latest states), and the address is pushed, so reload, Back, Forward and links
  // from elsewhere work. The sidebar itself stays (scroll, collapse, search, open groups). The
  // old chat's live stream goes with its content (htmx cleans the old elements up and the SSE
  // extension closes their EventSource); the new chat opens its own. What doesn't come back as
  // a part of a page (an error, a redirect to sign in, another kind of page) loads as a whole
  // page. Without script every link is a plain page load.
  var PAGE_PATHS = /^\/(chat|approvals|chats(\/\d+)?|agents\/[^/?#]+)?(\?|$)/;

  function here() {
    return window.location.pathname + window.location.search;
  }

  // `shown`: the page in the main column (null: a panel opened as the page, nothing under it).
  // `next`: a page to open once the panel above it has closed. `pending`: the one loading.
  // `focus`: move focus to the new page once it has settled. `from`: the page before it.
  var pageState = { shown: here(), next: null, pending: null, focus: false, from: null };

  // The page loaded as a page: its history entry is a page's (a panel's page has its own).
  function initPage() {
    if (!window.history.state && PAGE_PATHS.test(here())) {
      window.history.replaceState({ page: here() }, "");
    }
  }

  // The page a link leads to, if it is one of these: {url, hash}.
  function pageLink(a) {
    if (!a || a.target || a.hasAttribute("download") || a.hasAttribute("hx-get")) {
      return null;
    }
    var u;
    try {
      u = new URL(a.href, window.location.href);
    } catch (err) {
      return null;
    }
    var url = u.pathname + u.search;
    return u.origin === window.location.origin && PAGE_PATHS.test(url) ? { url: url, hash: u.hash } : null;
  }

  // In-page navigation needs the sidebar's page (not the sign-in or task pages), htmx, and no
  // voice message being recorded (a whole page load ends it, as before).
  function canSwapPage() {
    return !!(window.htmx && document.getElementById("main") && document.getElementById("sidebar")) &&
      !rec && !transcribing && !asking;
  }

  function pageScroller() {
    return document.scrollingElement || document.documentElement;
  }

  // Open `url` in the main column; `push`: a new history entry (else this one is replaced);
  // `scroll`: where the page was when it was left (Back, Forward).
  function openPage(url, push, hash, scroll) {
    hash = hash || "";
    if (!canSwapPage()) {
      window.location.assign(url + hash);
      return;
    }
    if (draftTimer !== null) {
      saveDraft();
    }
    var main = document.getElementById("main");
    // Remember where this page is scrolled (the phone's agent list scrolls the page).
    var st = window.history.state;
    if (push && st && st.page === pageState.shown) {
      window.history.replaceState({ page: st.page, scroll: pageScroller().scrollTop }, "");
    }
    // Only the last click counts.
    window.htmx.trigger(main, "htmx:abort");
    pageState.pending = { url: url, push: push, hash: hash, scroll: scroll };
    // Settled at once: htmx processes new content (the chat opens its stream) as it settles,
    // and a later settle could run after a quicker next page had already replaced it, opening
    // a stream for a chat that is gone.
    window.htmx.ajax("GET", url, { source: main, target: main, swap: "innerHTML settle:0ms" }).catch(function () {
      // Aborted by a newer click, or failed (htmx:sendError loads the page).
    });
  }

  function isPageSwap(e) {
    var d = e.detail;
    return !!(d && d.target && d.target.id === "main" && pageState.pending);
  }

  function wholePage() {
    var p = pageState.pending;
    pageState.pending = null;
    if (p) {
      window.location.assign(p.url + p.hash);
    }
  }

  // Not a part of a page: load it as a page (shows the error, the sign-in, ...).
  document.addEventListener("htmx:beforeSwap", function (e) {
    if (!isPageSwap(e)) {
      return;
    }
    var xhr = e.detail.xhr;
    if (!xhr || xhr.status !== 200 || /^\s*<(!doctype|html)/i.test(xhr.responseText || "")) {
      e.detail.shouldSwap = false;
      e.detail.isError = false;
      wholePage();
    }
  });

  document.addEventListener("htmx:sendError", function (e) {
    if (isPageSwap(e)) {
      wholePage();
    }
  });

  document.addEventListener("htmx:timeout", function (e) {
    if (isPageSwap(e)) {
      wholePage();
    }
  });

  document.addEventListener("htmx:afterSwap", function (e) {
    if (!isPageSwap(e) || e.target !== e.detail.target) {
      return;
    }
    var p = pageState.pending;
    pageState.pending = null;
    // Where it ended up (a page may redirect).
    var at = p.url;
    try {
      var u = new URL(e.detail.xhr.responseURL);
      if (u.origin === window.location.origin) {
        at = u.pathname + u.search;
      }
    } catch (err) {
      // Keep the requested URL.
    }
    pageState.from = pageState.shown;
    pageState.shown = at;
    if (p.push) {
      window.history.pushState({ page: at }, "", at + p.hash);
    } else {
      window.history.replaceState({ page: at }, "", at + p.hash);
    }
    // What the old page had open over the rest goes with it.
    each(document.querySelectorAll(".shell > .sidebar, .banner, .skip"), function (el) {
      el.inert = false;
    });
    each(document.querySelectorAll(".menu[popover]:popover-open"), function (m) {
      m.hidePopover();
    });
    // The new page starts at its top (on phones the page scrolls, else the main column), or
    // where it was when the user left it; a chat at its end.
    var main = e.detail.target;
    main.scrollTop = 0;
    pageScroller().scrollTop = typeof p.scroll === "number" ? p.scroll : 0;
    initMain(p.hash);
    pageState.focus = true;
  });

  // After the title changed (htmx sets it as the swap settles): the title a panel restores,
  // and focus on the new page's heading so screen readers start there (on the phone's agent
  // list, which hides the main column: on the open agent).
  document.addEventListener("htmx:afterSettle", function (e) {
    var main = e.detail && e.detail.target;
    if (!main || main.id !== "main" || e.target !== main || !pageState.focus) {
      return;
    }
    pageState.focus = false;
    panelState.title = document.title;
    var usable = function (el) {
      return !!el && el.getClientRects().length > 0 && !el.closest("[inert]");
    };
    var heading = Array.prototype.find.call(main.querySelectorAll("h1"), usable);
    var details = detailsParts();
    var to;
    if (heading) {
      heading.setAttribute("tabindex", "-1");
      to = heading;
    } else if (details && !isWide() && detailsOpen() && usable(details.panel)) {
      // The details sheet is open over the chat (remembered): it has the focus.
      to = details.panel;
    } else if (usable(main)) {
      to = main;
    } else {
      // The phone's agent list: the agent whose chat the user just left, else the open one.
      var left = pageState.from ? '#agent-groups .agent-link[href="' + CSS.escape(pageState.from) + '"]' : null;
      to = (left && document.querySelector(left)) ||
        document.querySelector("#agent-groups .agent-link[aria-current='page']") ||
        document.getElementById("agent-search");
    }
    if (to) {
      to.focus({ preventScroll: true });
    }
  });

  document.addEventListener("click", function (e) {
    if (e.defaultPrevented || e.button !== 0 || e.metaKey || e.ctrlKey || e.shiftKey || e.altKey) {
      return;
    }
    var t = e.target;
    var link = t instanceof Element ? pageLink(t.closest("a[href]")) : null;
    if (!link || !canSwapPage()) {
      return;
    }
    // A part of this page (#an-entry): the browser goes there.
    if (link.url === pageState.shown && link.hash) {
      return;
    }
    e.preventDefault();
    var o = overlayParts();
    if (o && o.dialog.open) {
      // From a panel: close it first (its history entries go), then open the page.
      pageState.next = link;
      closePanel();
      return;
    }
    openPage(link.url, link.url !== pageState.shown, link.hash);
  });

  // Back and Forward between pages (a panel's entries are the panel's own, above).
  window.addEventListener("popstate", function (e) {
    var next = pageState.next;
    if (next) {
      pageState.next = null;
      openPage(next.url, next.url !== pageState.shown, next.hash);
      return;
    }
    if (e.state && e.state.panel) {
      return;
    }
    var at = here();
    if (at === pageState.shown) {
      return;
    }
    if (PAGE_PATHS.test(at)) {
      openPage(at, false, window.location.hash, e.state && e.state.page === at ? e.state.scroll : undefined);
    } else {
      window.location.reload();
    }
  });

  // What a page's main column needs once it is in place (on load and after each in-page
  // navigation): the composer, its draft and files, the details panel, the chat at its end.
  function initMain(hash) {
    kept = [];
    clearPreviews();
    stickToBottom = true;
    grow(composer());
    showVoice();
    initDetails();
    restoreDraft();
    var to = hash ? document.getElementById(decodeURIComponent(hash.slice(1))) : null;
    var feed = document.getElementById("feed");
    if (to) {
      to.scrollIntoView({ block: "center" });
    } else if (feed && feed.getClientRects().length) {
      // (Not on the phone's agent list, which hides the chat.) Browsers with CSS
      // scroll-initial-target are there already on a page load.
      scrollToBottom();
    }
    // The chat may show now (melissa.css keeps it invisible until it is at its end where the
    // browser can't start it there by itself).
    document.documentElement.setAttribute("data-chat-ready", "");
    // After the first scroll, so the top of the feed isn't "in view" while the page loads.
    window.requestAnimationFrame(watchOlder);
  }

  // Settings search: filters the section list by name and the settings in each section, and
  // the rows of the open section. "/" focuses it while Settings is open.
  function initSettingsSearch(root) {
    var box = root.querySelector("[data-settings-search]");
    if (!box) {
      return;
    }
    box.hidden = false;
    var input = box.querySelector("input");
    if (input.value) {
      filterSettings(root, input.value);
    }
    input.addEventListener("input", function () {
      filterSettings(root, input.value);
    });
  }

  function filterSettings(root, query) {
    var q = query.trim().toLowerCase();
    var any = false;
    each(root.querySelectorAll(".settings-nav li"), function (li) {
      var text = (li.textContent + " " + (li.getAttribute("data-keywords") || "")).toLowerCase();
      li.hidden = q !== "" && text.indexOf(q) < 0;
      any = any || !li.hidden;
    });
    each(root.querySelectorAll(".settings-nav ul"), function (ul) {
      var shown = ul.querySelector("li:not([hidden])");
      ul.hidden = !shown;
      var label = ul.previousElementSibling;
      if (label && label.classList.contains("settings-group")) {
        label.hidden = !shown;
      }
    });
    var none = root.querySelector(".settings-none");
    if (none) {
      none.hidden = any;
    }
    each(root.querySelectorAll("[data-setting]"), function (row) {
      row.hidden = q !== "" && row.textContent.toLowerCase().indexOf(q) < 0;
    });
  }

  document.addEventListener("keydown", function (e) {
    if (e.key !== "/" || e.ctrlKey || e.metaKey || e.altKey) {
      return;
    }
    var o = overlayParts();
    var field = o && o.dialog.open && o.body.querySelector("[data-settings-search] input");
    var t = e.target;
    if (!field || (t instanceof Element && t.closest("input, textarea, select, [contenteditable]"))) {
      return;
    }
    e.preventDefault();
    field.focus();
  });

  // A section's content was swapped (a form inside the panel): keep the search applied.
  document.addEventListener("htmx:afterSettle", function () {
    var o = overlayParts();
    var input = o && o.dialog.open && o.body.querySelector("[data-settings-search] input");
    if (input && input.value) {
      filterSettings(o.body, input.value);
    }
  });

  // === Phone views (2026-11-20, owner design "Melissa — Mobile"; components::mobile) =========
  // The agent details sheet closes when its header is dragged down (the handle shows where);
  // "Review" in its Permissions brings the permission card into view, marks it for a moment
  // and puts focus on its first button (without script: the link's #anchor).

  var drag = null;
  var REDUCED = window.matchMedia ? window.matchMedia("(prefers-reduced-motion: reduce)") : null;

  document.addEventListener("pointerdown", function (e) {
    var t = e.target;
    if (!(t instanceof Element) || isWide() || !detailsOpen() || e.button !== 0) {
      return;
    }
    var head = t.closest(".details-head");
    if (!head || t.closest("button, a")) {
      return;
    }
    var panel = document.getElementById("details-panel");
    drag = { panel: panel, y: e.clientY, dy: 0, id: e.pointerId };
    panel.setAttribute("data-dragging", "");
    head.setPointerCapture(e.pointerId);
  });

  document.addEventListener("pointermove", function (e) {
    if (!drag || e.pointerId !== drag.id) {
      return;
    }
    drag.dy = Math.max(0, e.clientY - drag.y);
    drag.panel.style.transform = drag.dy ? "translateY(" + drag.dy + "px)" : "";
  });

  function endDrag(e) {
    if (!drag || e.pointerId !== drag.id) {
      return;
    }
    var d = drag;
    drag = null;
    d.panel.removeAttribute("data-dragging");
    d.panel.style.transform = "";
    // Far enough (a fifth of the sheet, at least 64px): close it.
    if (e.type === "pointerup" && d.dy > Math.max(64, d.panel.offsetHeight / 5)) {
      setDetails(false, true);
    }
  }

  document.addEventListener("pointerup", endDrag);
  document.addEventListener("pointercancel", endDrag);

  document.addEventListener("click", function (e) {
    var t = e.target;
    var review = t instanceof Element && t.closest("a[data-review]");
    if (!review) {
      return;
    }
    var href = review.getAttribute("href") || "";
    var at = href.indexOf("#");
    var entry = at >= 0 ? document.getElementById(href.slice(at + 1)) : null;
    if (!entry) {
      // Not in this chat's page (an older part, another chat): follow the link.
      return;
    }
    // (The details' own click handler already moved the sheet aside on narrow screens.)
    e.preventDefault();
    var card = entry.querySelector(".approval") || entry;
    card.scrollIntoView({ block: "center", behavior: REDUCED && REDUCED.matches ? "auto" : "smooth" });
    card.setAttribute("data-flash", "");
    setTimeout(function () {
      card.removeAttribute("data-flash");
    }, 1600);
    var first = card.querySelector(".actions button");
    if (first) {
      first.focus({ preventScroll: true });
    }
  });

  // === End of phone views =====================================================================

  document.addEventListener("DOMContentLoaded", function () {
    initDismiss();
    initPanel();
    initPage();
    initSidebar();
    initMain("");
  });
})();
