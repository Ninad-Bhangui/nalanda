-- A library written by Nalanda 0.0.1 (sqlite3 nalanda.db .dump): an EPUB with a PNG
-- cover converted to KEPUB, and a TXT book whose metadata was edited.
CREATE TABLE books (
    id           INTEGER PRIMARY KEY,
    title        TEXT NOT NULL,
    sort_title   TEXT NOT NULL,
    authors      TEXT NOT NULL,      -- JSON array
    author_sort  TEXT NOT NULL,
    series       TEXT,
    series_index REAL,
    publisher    TEXT,
    language     TEXT,
    published    TEXT,
    description  TEXT,
    tags         TEXT NOT NULL,      -- JSON array
    identifiers  TEXT NOT NULL,      -- JSON object
    path         TEXT NOT NULL,      -- directory relative to the library root
    cover        TEXT,               -- cover file name inside `path`
    added_at     TEXT NOT NULL,
    modified_at  TEXT NOT NULL
);
INSERT INTO books VALUES(1,'Solaris','Solaris','["Stanisław Lem"]','Lem, Stanisław',NULL,NULL,NULL,'pl',NULL,NULL,'[]','{}','Stanisław Lem/Solaris (1)','cover.png','2026-10-16T08:46:35.589422026+00:00','2026-10-16T08:46:35.626333434+00:00');
INSERT INTO books VALUES(2,'Walden','Walden','["Henry David Thoreau"]','Thoreau, Henry David','Essays',1.0,'Ticknor','en','1854-08-09','Life in the woods.','["Nature","Philosophy"]','{"isbn":"9780486284958"}','Unknown/Walden (2)',NULL,'2026-10-16T08:46:35.608058199+00:00','2026-10-16T08:46:35.646580540+00:00');
CREATE TABLE formats (
    book_id   INTEGER NOT NULL REFERENCES books(id) ON DELETE CASCADE,
    format    TEXT NOT NULL,
    file_name TEXT NOT NULL,
    size      INTEGER NOT NULL,
    PRIMARY KEY (book_id, format)
);
INSERT INTO formats VALUES(1,'epub','Solaris - Stanisław Lem.epub',1655);
INSERT INTO formats VALUES(2,'txt','Walden - Unknown.txt',28);
INSERT INTO formats VALUES(1,'kepub.epub','Solaris - Stanisław Lem.kepub.epub',1655);
