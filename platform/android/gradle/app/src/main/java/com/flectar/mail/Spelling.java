package com.flectar.mail;

import android.content.ActivityNotFoundException;
import android.content.Context;
import android.content.Intent;
import android.os.Build;
import android.os.Handler;
import android.os.Looper;
import android.view.textservice.SentenceSuggestionsInfo;
import android.view.textservice.SpellCheckerSession;
import android.view.textservice.SuggestionsInfo;
import android.view.textservice.TextInfo;
import android.view.textservice.TextServicesManager;

import java.util.ArrayDeque;
import java.util.ArrayList;
import java.util.Iterator;
import java.util.concurrent.CompletableFuture;
import java.util.concurrent.TimeUnit;
import java.util.concurrent.TimeoutException;

/**
 * Spelling for platform/android/src/spelling.rs, with the spell checker the user chose in
 * Android's settings, usually the keyboard's, which checks in its own process and in the
 * language set there. Rust asks from its own threads and waits. The session lives on the main
 * thread, where Android answers, as it does for text fields.
 */
final class Spelling {
    /** As many corrections as Android's text fields offer. */
    private static final int SUGGESTIONS = 5;
    /**
     * How long a request waits for its answer. A spell checker may take a while to start, and
     * one that started and then answers nothing is started again.
     */
    private static final long ANSWER_MS = 20_000;
    /** Android's "Add to dictionary" dialog, which its text fields open. */
    private static final String ADD_WORD = "com.android.settings.USER_DICTIONARY_INSERT";

    private final Context context;
    private final Handler main = new Handler(Looper.getMainLooper());
    /** The session, and the requests it hasn't answered yet, used on the main thread only. */
    private SpellCheckerSession session;
    private final ArrayDeque<Request> asked = new ArrayDeque<>();
    private int sequence;

    Spelling(Context context) {
        this.context = context.getApplicationContext();
    }

    /** Whether Android checks spelling for apps now. Before Android 12 it doesn't say. */
    static boolean available(Context context) {
        if (Build.VERSION.SDK_INT < Build.VERSION_CODES.S) {
            return true;
        }
        TextServicesManager manager = context.getSystemService(TextServicesManager.class);
        return manager != null
                && manager.isSpellCheckerEnabled()
                && manager.getCurrentSpellCheckerInfo() != null;
    }

    /**
     * The misspelled words of {@code text} as UTF-16 start and end offsets, one pair after
     * another, or null when Android checks no spelling now.
     */
    synchronized int[] check(String text) throws Exception {
        SentenceSuggestionsInfo[] results = ask(text);
        if (results == null) {
            return null;
        }
        ArrayList<Integer> found = new ArrayList<>();
        for (SentenceSuggestionsInfo result : results) {
            // Some spell checkers answer null for a part.
            if (result == null) {
                continue;
            }
            for (int i = 0; i < result.getSuggestionsCount(); i++) {
                SuggestionsInfo info = result.getSuggestionsInfoAt(i);
                int start = result.getOffsetAt(i);
                int end = start + result.getLengthAt(i);
                if (info != null && misspelled(info) && 0 <= start && start < end
                        && end <= text.length()) {
                    found.add(start);
                    found.add(end);
                }
            }
        }
        int[] ranges = new int[found.size()];
        for (int i = 0; i < ranges.length; i++) {
            ranges[i] = found.get(i);
        }
        return ranges;
    }

    /** Corrections for {@code word}, the likeliest first. */
    synchronized String[] suggest(String word) throws Exception {
        SentenceSuggestionsInfo[] results = ask(word);
        ArrayList<String> suggestions = new ArrayList<>();
        if (results == null) {
            return new String[0];
        }
        for (SentenceSuggestionsInfo result : results) {
            if (result == null) {
                continue;
            }
            for (int i = 0; i < result.getSuggestionsCount(); i++) {
                SuggestionsInfo info = result.getSuggestionsInfoAt(i);
                for (int j = 0; info != null && j < info.getSuggestionsCount(); j++) {
                    String suggestion = info.getSuggestionAt(j);
                    // Some spell checkers end a suggestion with a zero-width space.
                    if (suggestion != null && suggestion.endsWith("\u200b")) {
                        suggestion = suggestion.substring(0, suggestion.length() - 1);
                    }
                    if (suggestion != null && !suggestion.isEmpty()
                            && !suggestions.contains(suggestion)) {
                        suggestions.add(suggestion);
                    }
                }
            }
        }
        return suggestions.toArray(new String[0]);
    }

    /** Open Android's dialog that adds {@code word} to the user's dictionary. False without one. */
    boolean addWord(String word) {
        Intent intent = new Intent(ADD_WORD)
                .putExtra("word", word)
                .addFlags(Intent.FLAG_ACTIVITY_NEW_TASK);
        try {
            context.startActivity(intent);
            return true;
        } catch (ActivityNotFoundException e) {
            return false;
        }
    }

    /** End the session. The next request starts one, with the settings then. */
    void close() {
        main.post(this::end);
    }

    /** A word Android's text fields would underline. */
    private static boolean misspelled(SuggestionsInfo info) {
        int attributes = info.getSuggestionsAttributes();
        return (attributes & SuggestionsInfo.RESULT_ATTR_LOOKS_LIKE_TYPO) != 0
                && (attributes & SuggestionsInfo.RESULT_ATTR_IN_THE_DICTIONARY) == 0;
    }

    /** The spell checker's answer about {@code text}, or null when it checks nothing now. */
    private SentenceSuggestionsInfo[] ask(String text) throws Exception {
        CompletableFuture<SentenceSuggestionsInfo[]> answer = new CompletableFuture<>();
        main.post(() -> {
            if (session == null) {
                start();
            }
            if (session == null) {
                answer.complete(null);
                return;
            }
            Request request = new Request(++sequence, answer);
            asked.add(request);
            // A full stop at the very end counts as part of the word before it, so a space
            // follows.
            session.getSentenceSuggestions(
                    new TextInfo[] {new TextInfo(text + " ", 0, request.sequence)}, SUGGESTIONS);
        });
        try {
            return answer.get(ANSWER_MS, TimeUnit.MILLISECONDS);
        } catch (TimeoutException e) {
            // A spell checker still starting answers later, and the answer goes to the request
            // that asked. One that started and didn't answer is started again.
            main.post(() -> {
                if (session != null && !session.isSessionDisconnected()) {
                    end();
                }
            });
            throw e;
        }
    }

    /**
     * Start a session with the spell checker and language set in Android's settings, as
     * Chromium does. There is none when the user turned spell checking off.
     */
    private void start() {
        TextServicesManager manager = context.getSystemService(TextServicesManager.class);
        if (manager == null) {
            return;
        }
        Answers answers = new Answers();
        session = manager.newSpellCheckerSession(null, null, answers, true);
        answers.session = session;
    }

    private void end() {
        if (session != null) {
            session.cancel();
            session.close();
            session = null;
        }
        IllegalStateException closed = new IllegalStateException("the spell checker closed");
        for (Request request : asked) {
            request.answer.completeExceptionally(closed);
        }
        asked.clear();
    }

    /** The request {@code results} answer: the one with their sequence number, else the oldest. */
    private Request answered(SentenceSuggestionsInfo[] results) {
        for (int i = 0; results != null && i < results.length; i++) {
            SuggestionsInfo info = results[i] == null || results[i].getSuggestionsCount() <= 0
                    ? null : results[i].getSuggestionsInfoAt(0);
            if (info == null) {
                continue;
            }
            for (Iterator<Request> requests = asked.iterator(); requests.hasNext(); ) {
                Request request = requests.next();
                if (request.sequence == info.getSequence()) {
                    requests.remove();
                    return request;
                }
            }
        }
        return asked.poll();
    }

    /** A request to the spell checker, numbered so its answer finds it. */
    private static final class Request {
        final int sequence;
        final CompletableFuture<SentenceSuggestionsInfo[]> answer;

        Request(int sequence, CompletableFuture<SentenceSuggestionsInfo[]> answer) {
            this.sequence = sequence;
            this.answer = answer;
        }
    }

    /** One session's answers. A session that ended answers no one. */
    private final class Answers implements SpellCheckerSession.SpellCheckerSessionListener {
        SpellCheckerSession session;

        @Override
        public void onGetSuggestions(SuggestionsInfo[] results) {}

        @Override
        public void onGetSentenceSuggestions(SentenceSuggestionsInfo[] results) {
            if (session != Spelling.this.session) {
                return;
            }
            Request request = answered(results);
            if (request != null) {
                request.answer.complete(results);
            }
        }
    }
}
