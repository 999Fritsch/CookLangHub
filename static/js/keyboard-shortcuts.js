/*
 * Keyboard shortcuts, and the card that lists them.
 *
 * The shape is CookCLI's: `/` reaches the search, `g` and a letter go
 * somewhere, `?` shows the list, and Escape closes it. The letters differ,
 * because the places differ.
 *
 * CookCLI opens their card from an `onclick` attribute. The Content Security
 * Policy here is `default-src 'self'`, which refuses an inline handler and an
 * inline style, so the trigger is a `data-shortcuts` attribute and the card
 * is built from classes alone.
 *
 * Nothing here is needed to use the application. Every shortcut goes to a
 * link or a button that a person can also reach with Tab.
 *
 * CookCLI: Copyright (c) 2021-2023 Alexey Dubovskoy, MIT. See NOTICE.
 */
(function () {
    'use strict';

    var pendingKey = null;
    var pendingTimer = null;

    /* Where `g` and a letter go. */
    var PLACES = {
        r: ['/', 'Recipes'],
        c: ['/cookbooks', 'Cookbooks'],
        s: ['/suggestions', 'Suggestions'],
        e: ['/explore', 'Explore'],
        n: ['/recipes/new', 'New Recipe'],
        p: ['/preferences', 'Preferences']
    };

    /*
     * What actually holds the caret.
     *
     * The editor is CodeMirror inside a shadow root, and a key pressed there
     * reaches this listener with the target changed to the element that
     * holds the root. Asking that element what it is inside answers about
     * the page and not about the editor, so the answer was always "not
     * writing" and every shortcut fired while a person typed a Recipe.
     */
    function deepestActive() {
        var element = document.activeElement;
        while (element && element.shadowRoot && element.shadowRoot.activeElement) {
            element = element.shadowRoot.activeElement;
        }
        return element;
    }

    /* A person who is writing must keep every key they press. */
    function isWriting(target) {
        if (!target) return false;
        var tag = (target.tagName || '').toLowerCase();
        if (tag === 'textarea' || tag === 'select') return true;
        if (tag === 'input') {
            var type = (target.type || 'text').toLowerCase();
            return ['text', 'search', 'password', 'email', 'number', 'url', 'tel',
                    'date', 'time', 'datetime-local'].indexOf(type) !== -1;
        }
        if (target.isContentEditable) return true;
        /* The editor is CodeMirror inside a shadow root. */
        if (target.closest && target.closest('.cm-editor')) return true;
        return false;
    }

    function clearPending() {
        pendingKey = null;
        if (pendingTimer) { clearTimeout(pendingTimer); pendingTimer = null; }
    }

    function row(label, keys) {
        var line = document.createElement('div');
        line.className = 'flex justify-between items-center gap-4';
        var name = document.createElement('span');
        name.className = 'text-gray-600';
        name.textContent = label;
        var holder = document.createElement('span');
        holder.className = 'flex gap-1';
        keys.forEach(function (key) {
            var tag = document.createElement('kbd');
            tag.className = 'px-2 py-1 bg-gray-100 rounded text-sm font-mono';
            tag.textContent = key;
            holder.appendChild(tag);
        });
        line.appendChild(name);
        line.appendChild(holder);
        return line;
    }

    function group(title, rows) {
        var box = document.createElement('div');
        var head = document.createElement('h3');
        head.className = 'font-bold mb-2 text-gray-900';
        head.textContent = title;
        box.appendChild(head);
        var list = document.createElement('div');
        list.className = 'space-y-2';
        rows.forEach(function (r) { list.appendChild(r); });
        box.appendChild(list);
        return box;
    }

    function build() {
        var card = document.createElement('div');
        card.id = 'keyboard-shortcuts';
        card.className = 'fixed inset-0 z-50 flex items-center justify-center p-4 bg-black/50';
        card.setAttribute('role', 'dialog');
        card.setAttribute('aria-modal', 'true');
        card.setAttribute('aria-label', 'Keyboard shortcuts');

        var inner = document.createElement('div');
        inner.className = 'bg-white rounded-2xl shadow-lg p-6 max-w-lg w-full max-h-[80vh] overflow-y-auto';

        var title = document.createElement('h2');
        title.className = 'text-xl font-bold mb-4 text-gray-900';
        title.textContent = 'Keyboard shortcuts';
        inner.appendChild(title);

        var body = document.createElement('div');
        body.className = 'space-y-6';

        body.appendChild(group('Anywhere', [
            row('Search the Recipe titles', ['/']),
            row('Show this card', ['?']),
            row('Close this card', ['Esc'])
        ]));

        var places = Object.keys(PLACES).map(function (key) {
            return row('Go to ' + PLACES[key][1], ['g', key]);
        });
        body.appendChild(group('Go to', places));

        if (document.querySelector('[data-start-cooking]')) {
            body.appendChild(group('This Recipe', [row('Start cooking', ['c'])]));
        }

        inner.appendChild(body);

        var close = document.createElement('button');
        close.type = 'button';
        close.className = 'mt-6 px-5 py-2 rounded-full font-medium border border-gray-300 text-gray-700 hover:bg-gray-50';
        close.textContent = 'Close';
        close.addEventListener('click', hide);
        inner.appendChild(close);

        card.appendChild(inner);
        card.addEventListener('click', function (event) {
            if (event.target === card) hide();
        });
        document.body.appendChild(card);
        return card;
    }

    var lastFocus = null;

    function show() {
        var card = document.getElementById('keyboard-shortcuts') || build();
        card.classList.remove('hidden');
        lastFocus = document.activeElement;
        var close = card.querySelector('button');
        if (close) close.focus();
    }

    function hide() {
        var card = document.getElementById('keyboard-shortcuts');
        if (!card) return;
        card.classList.add('hidden');
        if (lastFocus && lastFocus.focus) lastFocus.focus();
    }

    function open() {
        var card = document.getElementById('keyboard-shortcuts');
        return card && !card.classList.contains('hidden');
    }

    document.addEventListener('click', function (event) {
        var trigger = event.target.closest && event.target.closest('[data-shortcuts]');
        if (trigger) { event.preventDefault(); show(); }
    });

    document.addEventListener('keydown', function (event) {
        if (event.key === 'Escape') {
            if (open()) { event.preventDefault(); hide(); return; }
            var writing = deepestActive();
            if (isWriting(writing) && writing.blur) writing.blur();
            return;
        }

        if (isWriting(deepestActive()) || isWriting(event.target)) return;
        if (event.ctrlKey || event.metaKey || event.altKey) return;

        if (pendingKey === 'g') {
            var place = PLACES[event.key];
            clearPending();
            if (place) { event.preventDefault(); window.location.href = place[0]; return; }
        }

        if (event.key === '/') {
            var search = document.getElementById('nav-search');
            if (search) { event.preventDefault(); search.focus(); search.select(); }
            return;
        }

        if (event.key === '?') { event.preventDefault(); show(); return; }

        if (event.key === 'g') {
            pendingKey = 'g';
            pendingTimer = setTimeout(clearPending, 1500);
            return;
        }

        if (event.key === 'c') {
            var cook = document.querySelector('[data-start-cooking]');
            if (cook) { event.preventDefault(); cook.click(); }
        }
    });
})();
