# Course format

One file per course, named `<id>.json` (the `id` inside must match the file name).
The server reloads changed files automatically and the app downloads them on its next sync.

```jsonc
{
  "id": "en",                 // [a-z0-9_-], must match the file name
  "version": 1,               // informational; bump it when you publish changes
  "name": "Englisch",         // shown in the app
  "flag": "🇬🇧",
  "tts": "en-GB",             // language tag for text-to-speech
  "chars": [],                // extra keys shown when typing in this language, e.g. ["é", "ç"]
  "keepCap": ["I"],           // words that always keep their capital letter in word banks
  "lessonSize": 4,            // optional, sentences per lesson (default 4)
  "order": 10,                // optional, position in the course list
  "published": true,          // optional, false = hidden from the app (draft)
  "skills": [
    {
      "id": "g1",             // never change an id once people have progress on it
      "title": "Grundlagen 1",
      "icon": "🥚",
      "row": 0,               // skills in the same row sit side by side; a row unlocks when all rows above are done
      "level": "A1",          // optional; the app shows a "Stufe A1 · Einstieg" heading where a new level starts
      "color": "#1cb0f6",
      "words": [["man", "Mann"], ["woman", "Frau"]],
      "sentences": [
        { "t": ["I am a man.", "I'm a man."], "de": ["Ich bin ein Mann."] }
      ]
    }
  ],
  "gloss":   { "am": "bin" },          // tap-hints for target-language words (lower case)
  "glossDe": { "bin": "am" }           // tap-hints for German words
}
```

* `t` holds accepted answers in the target language, `de` accepted German answers. The **first** entry is what
  the learner sees and what word banks are built from; further entries are accepted alternatives.
* Words from `words` are used for matching and "which of these means …" exercises and automatically become hints.
* Adding sentences to a skill can add a lesson. Learners who had finished the skill then see it as unfinished
  again, which is usually what you want for new material.
