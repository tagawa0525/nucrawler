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
    // 読み直し（resync）が、この送信より前の状態で上書きしないように、押した時刻を残す
    marks.dataset.changed = String(performance.now());
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
  // 戻るボタンで戻ると、ブラウザは詳細を開く前のページを出す（詳細で付いた既読や評価が映らない）。
  // カードの記事の印（評価・ブックマーク・既読）だけを読み直して、見た目を今の状態に合わせる
  const resync = async () => {
    const started = performance.now();
    const cards = new Map(
      [...document.querySelectorAll(".card[data-id]")].map((c) => [c.dataset.id, c]),
    );
    if (cards.size === 0) return;
    const res = await fetch(`/api/marks?ids=${[...cards.keys()].join(",")}`, { cache: "no-store" })
      .then((r) => (r.ok ? r.json() : null))
      .catch(() => null);
    if (!res) return;
    for (const m of res.marks) {
      const card = cards.get(String(m.id));
      const marks = card && card.querySelector(".marks");
      // 読み直しを始めた後に押された印は、その送信の結果のほうが新しい
      if (!marks || Number(marks.dataset.changed ?? -1) >= started) continue;
      setStars(marks.querySelector(".rating"), m.rating ?? 0);
      setToggle(marks.querySelector('form[action$="/bookmark"] button'), m.bookmarked);
      setRead(marks, m.read);
    }
  };
  addEventListener("pageshow", (e) => {
    const nav = performance.getEntriesByType("navigation")[0];
    if (e.persisted || (nav && nav.type === "back_forward")) resync();
  });
})();
