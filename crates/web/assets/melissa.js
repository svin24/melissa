// Melissa's own script: small conveniences on top of htmx. Served from this origin (the CSP
// forbids inline code); no eval, no markup building (file chips are copies of a server-rendered
// template). Everything works without it, except Enter-to-send, automatic scrolling, pasting
// or dropping files, previews of picked files (the attach button still works) and voice input
// (its button stays hidden).
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

  document.addEventListener("DOMContentLoaded", function () {
    grow(composer());
    showVoice();
    if (document.getElementById("feed")) {
      scrollToBottom();
    }
  });
})();
