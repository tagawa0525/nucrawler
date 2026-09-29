(() => {
  // 失敗だけを知らせる（成功はボタンの状態で分かる）。スクリーンリーダーにも伝える
  const toast = document.createElement("div");
  toast.className = "toast";
  toast.setAttribute("role", "status");
  toast.hidden = true;
  document.body.append(toast);
  let timer;
  const notify = (text) => {
    clearTimeout(timer);
    toast.textContent = text;
    toast.hidden = false;
    timer = setTimeout(() => { toast.hidden = true; }, 6000);
  };
  const setToggle = (button, on) => {
    button.classList.toggle("on", on);
    button.value = on ? "0" : "1";
    button.setAttribute("aria-pressed", String(on));
  };
  // 評価 `rating`（評価なしは 0）に合わせて星を塗り直す。今の評価の星は、押すと評価なしに戻す
  const setStars = (form, rating) => {
    form.querySelectorAll("button").forEach((b, i) => {
      const n = i + 1;
      const on = n <= rating;
      b.classList.toggle("on", on);
      b.textContent = on ? "★" : "☆";
      b.value = n === rating ? "" : String(n);
      const title = b.dataset.label + (n === rating ? "（押すと評価なし）" : "");
      b.title = title;
      b.setAttribute("aria-label", title);
    });
  };
  const setRead = (marks, on) => {
    const button = marks.querySelector('form[action$="/read"] button');
    if (button) setToggle(button, on);
    const card = marks.closest(".card");
    if (card) card.classList.toggle("read", on);
  };
  // 送っている間は同じ印を送り直さない（押した結果が前後する）
  const busy = new WeakSet();
  document.addEventListener("submit", async (e) => {
    const form = e.target;
    const marks = form.closest(".marks");
    if (!marks || !e.submitter) return;
    e.preventDefault();
    if (busy.has(form)) return;
    busy.add(form);
    const { name, value } = e.submitter;
    const ok = await fetch(form.action, {
      method: "POST",
      headers: { "content-type": "application/x-www-form-urlencoded" },
      body: `${name}=${encodeURIComponent(value)}`,
      redirect: "manual",
    }).then((res) => res.ok || res.type === "opaqueredirect").catch(() => false);
    busy.delete(form);
    if (!ok) {
      notify("記録できませんでした");
      return;
    }
    if (form.classList.contains("rating")) {
      setStars(form, value === "" ? 0 : Number(value));
      // 評価すると既読になる
      if (value !== "") setRead(marks, true);
    } else if (form.action.endsWith("/read")) {
      setRead(marks, value === "1");
    } else {
      setToggle(e.submitter, value === "1");
    }
  });
  // カードの印のボタンを押す（スワイプ・キーもこの送信を通す）
  const press = (card, selector) => {
    const button = card.querySelector(selector);
    if (button) button.form.requestSubmit(button);
  };
  const BOOKMARK = 'form[action$="/bookmark"] button';
  const READ = 'form[action$="/read"] button';
  const MARK_KEYS = { l: BOOKMARK, ArrowRight: BOOKMARK, h: READ, ArrowLeft: READ };
  const MOVE_KEYS = { j: 1, ArrowDown: 1, k: -1, ArrowUp: -1 };
  document.addEventListener("keydown", (e) => {
    if (e.altKey || e.ctrlKey || e.metaKey || e.target.closest("input, textarea, select")) return;
    const cards = [...document.querySelectorAll(".card[data-id]")];
    const current = e.target.closest(".card[data-id]");
    const at = cards.indexOf(current);
    if (e.key in MOVE_KEYS) {
      const next = cards[at < 0 ? 0 : Math.min(Math.max(at + MOVE_KEYS[e.key], 0), cards.length - 1)];
      if (next) { e.preventDefault(); next.focus(); }
    } else if (!current) {
      return;
    } else if (e.key in MARK_KEYS) {
      e.preventDefault();
      press(current, MARK_KEYS[e.key]);
    } else if (e.key >= "1" && e.key <= "5") {
      e.preventDefault();
      press(current, `.rating button:nth-child(${e.key})`);
    } else if (e.key === "0") {
      // 今の評価の星（押すと評価なし）
      e.preventDefault();
      press(current, '.rating button[value=""]');
    } else if (e.key === "Enter" && e.target === current) {
      current.querySelector("a.title").click();
    }
  });
  const EDGE = 24, START = 10, COMMIT = 0.35;
  for (const card of document.querySelectorAll(".card[data-id]")) {
    let x0 = null, y0 = 0, dx = 0, dragging = false, moved = false;
    card.addEventListener("pointerdown", (e) => {
      if (e.button !== 0 || e.target.closest("button")) return;
      if (e.clientX < EDGE || e.clientX > innerWidth - EDGE) return;
      x0 = e.clientX; y0 = e.clientY; dx = 0; dragging = false; moved = false;
    });
    card.addEventListener("pointermove", (e) => {
      if (x0 === null) return;
      dx = e.clientX - x0;
      if (!dragging) {
        if (Math.abs(e.clientY - y0) > Math.abs(dx)) { x0 = null; return; }
        if (Math.abs(dx) < START) return;
        dragging = moved = true;
        // マウスで引いたときに本文が選択されないようにする
        getSelection().removeAllRanges();
        card.style.userSelect = "none";
        card.setPointerCapture(e.pointerId);
        card.style.transition = "none";
      }
      card.style.transform = `translateX(${dx}px)`;
      card.dataset.dir = dx > 0 ? "bookmark" : "read";
    });
    // 離したら元の位置に戻し、十分に動かしていれば印を切り替える（カードは消さない）
    const end = (commit) => {
      const was = dragging;
      x0 = null; dragging = false;
      if (!was) return;
      card.style.transition = "";
      card.style.transform = "";
      card.style.userSelect = "";
      delete card.dataset.dir;
      if (commit && Math.abs(dx) > card.offsetWidth * COMMIT) {
        press(card, dx > 0 ? BOOKMARK : READ);
      }
    };
    card.addEventListener("pointerup", () => end(true));
    card.addEventListener("pointercancel", () => end(false));
    // スワイプの指を離したときのクリックで、記事を開かない
    card.addEventListener("click", (e) => {
      if (moved) { e.preventDefault(); moved = false; }
    }, true);
  }
})();
