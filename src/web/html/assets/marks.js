(() => {
  // 失敗と、カードを隠したことを知らせる（ほかの成功はボタンの状態で分かる）。スクリーンリーダーにも伝える
  const toast = document.createElement("div");
  toast.className = "toast";
  toast.setAttribute("role", "status");
  toast.hidden = true;
  document.body.append(toast);
  let timer, undoLast = null;
  const hideToast = () => { toast.hidden = true; undoLast = null; };
  // `undo` があれば「元に戻す」を添える（u キーでも戻す）
  const notify = (text, undo) => {
    clearTimeout(timer);
    toast.textContent = text;
    undoLast = undo || null;
    if (undo) {
      const button = document.createElement("button");
      button.textContent = "元に戻す";
      button.onclick = () => { hideToast(); undo(); };
      toast.append(button);
    }
    toast.hidden = false;
    timer = setTimeout(hideToast, 6000);
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
  const shownCards = () => [...document.querySelectorAll(".card[data-id]")].filter((c) => !c.hidden);
  // 既読を隠す一覧（data-hide-read）のカードを、既読なら隠し、未読なら出す。隠したカードを
  // 選んでいたなら隣を選び、隠したかどうかを返す
  const setVisibility = (card, read) => {
    if (!card.closest("[data-hide-read]")) return false;
    if (!read) {
      card.hidden = false;
      return false;
    }
    if (card.hidden) return false;
    if (card.contains(document.activeElement)) {
      const cards = shownCards();
      const at = cards.indexOf(card);
      const next = cards[at + 1] || cards[at - 1];
      if (next) next.focus();
    }
    card.hidden = true;
    return true;
  };
  // 既読の印で既読にしたカードを隠し、しばらく「元に戻す」を出す
  const hideRead = (marks) => {
    const card = marks.closest(".card[data-id]");
    if (!card) return;
    const focused = card.contains(document.activeElement);
    if (!setVisibility(card, true)) return;
    notify("既読にしました", () => {
      card.hidden = false;
      if (focused) card.focus();
      const button = card.querySelector(READ);
      if (button) button.form.requestSubmit(button);
    });
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
    // 読み直し（resync）が、この送信より前の状態で上書きしないように、押した印ごとに時刻を残す
    form.dataset.changed = String(performance.now());
    const ok = await fetch(form.action, {
      method: "POST",
      headers: { "content-type": "application/x-www-form-urlencoded" },
      body: `${name}=${encodeURIComponent(value)}`,
      redirect: "manual",
    }).then((res) => res.ok || res.type === "opaqueredirect").catch(() => false);
    busy.delete(form);
    if (!ok) {
      notify("記録できませんでした");
      // 送れたか分からないので、印を今の状態に合わせ直す。この読み直しは押した後に始まるので、
      // 押した印もサーバーの状態で上書きする（押した印を残すのは、押す前に始まった読み直しだけ）
      resync();
      return;
    }
    if (form.classList.contains("rating")) {
      setStars(form, value === "" ? 0 : Number(value));
    } else if (form.action.endsWith("/read")) {
      setRead(marks, value === "1");
      if (value === "1") {
        hideRead(marks);
      } else {
        // 元に戻すの送信中に始まった読み直しが、古い既読の状態でカードを隠していても出し直す
        const card = marks.closest(".card[data-id]");
        if (card) setVisibility(card, false);
      }
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
    const cards = shownCards();
    const current = e.target.closest(".card[data-id]");
    const at = cards.indexOf(current);
    if (e.key in MOVE_KEYS) {
      const next = cards[at < 0 ? 0 : Math.min(Math.max(at + MOVE_KEYS[e.key], 0), cards.length - 1)];
      if (next) { e.preventDefault(); next.focus(); }
    } else if (e.key === "u" && undoLast) {
      // 隠したカードを戻す（カードを選んでいなくても効く）
      e.preventDefault();
      const undo = undoLast;
      hideToast();
      undo();
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
    // 離したら元の位置に戻し、十分に動かしていれば印を切り替える
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
  // `/api/marks` が 1 回に受け付ける件数の上限（サーバーの MAX_MARK_IDS と同じ）
  const MAX_MARK_IDS = 500;
  const resync = async () => {
    const started = performance.now();
    const cards = new Map(
      [...document.querySelectorAll(".card[data-id]")].map((c) => [c.dataset.id, c]),
    );
    // 受付の上限（1 回 500 件）ごとに分けて問い合わせる
    const ids = [...cards.keys()];
    for (let i = 0; i < ids.length; i += MAX_MARK_IDS) {
      const batch = ids.slice(i, i + MAX_MARK_IDS).join(",");
      const res = await fetch(`/api/marks?ids=${batch}`, { cache: "no-store" })
        .then((r) => (r.ok ? r.json() : null))
        .catch(() => null);
      if (res) apply(cards, res.marks, started);
    }
  };
  // 読み直した印を、読み直しを始めた後に押されていない印にだけ当てる（押した印は、その送信の結果のほうが新しい）
  const apply = (cards, list, started) => {
    const untouched = (form) => form && Number(form.dataset.changed ?? -1) < started;
    for (const m of list) {
      const card = cards.get(String(m.id));
      const marks = card && card.querySelector(".marks");
      if (!marks) continue;
      const rating = marks.querySelector(".rating");
      if (untouched(rating)) setStars(rating, m.rating ?? 0);
      const bookmark = marks.querySelector('form[action$="/bookmark"]');
      if (untouched(bookmark)) setToggle(bookmark.querySelector("button"), m.bookmarked);
      if (untouched(marks.querySelector('form[action$="/read"]'))) {
        setRead(marks, m.read);
        // 詳細を開いて既読になった記事は、戻ったときに隠す
        setVisibility(card, m.read);
      }
    }
  };
  addEventListener("pageshow", (e) => {
    const nav = performance.getEntriesByType("navigation")[0];
    if (e.persisted || (nav && nav.type === "back_forward")) resync();
  });
})();
