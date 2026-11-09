# בדיקות חיות ל-PRs של 2026-11-09

כל PR ברשימה קומפל ב-CI בלבד. אף PR לא רץ על מכונה אמיתית (Rule #14). סדר הרשימה הוא סדר הסיכון, החמור ראשון.

## אזהרה לפני שמתחילים

אל תשלח הערות diff (#95) ל-pane של zellij עם shell רגיל לפני בדיקה עם תוכן לא מזיק. ייתכן ש-zellij מסתיר הדבקה בסוגריים, ואז כל שורה תרוץ כפקודה.

## #100 `russh` 0.65.1 (תלוי ב-#92), Windows ו-macOS

1. התחבר ל-host מוכר.
2. שנה את טביעת האצבע ב-`known_hosts.json`. החיבור חייב להיכשל עם בקשת mismatch.
3. מחק את הרשומה. בקשת unknown-host חייבת להופיע, ואישור יוצר את הרשומה מחדש.
4. התחבר עם מפתח ed25519, מפתח RSA וסיסמה. נסה מפתח מוצפן עם passphrase נכון ושגוי.
5. התחבר עם RSA דרך agent: OpenSSH agent ו-Pageant ב-Windows, `SSH_AUTH_SOCK` ב-macOS.
6. הרץ hook של `ymux` בשרת, וודא שהוא מגיע ל-RPC דרך ה-reverse tunnel.
7. שנה גודל של pane בזמן פלט רץ.
8. הדבק יותר מ-2 MiB בזמן פלט מהשרת. הסשן חייב לא להיתקע.
9. פתח auto port-forward, השהה הורדה גדולה בדפדפן והקלד ב-pane. כך רואים את התקיעה הידועה.
10. בדוק העלאה מעל 10 MB ב-File Manager, "Test connect" ו-wizard ה-provisioning.
11. התחבר לשרת הישן ביותר שאתה עדיין משתמש בו.

## #92 ליבת SSH exec

1. Hooks banner ואז Update now: ה-toast חייב להתאים לתוצאה. בדוק גם שרת בלי גישה ל-GitHub.
2. Wizard ה-provisioning על שרת זמני, כולל שלב apt ו-Docker.
3. Connect לשרת קיים והתקנת מפתח.
4. על SSH: zip, tar, unzip ושמירת ticket.
5. נעץ תיקייה שאינה repo על host של SSH.
6. סגור ושנה שם של סשן tmux.
7. פתח File Manager או port forward ב-workspace שיש בו רק sibling session.

## #91 merge ו-single-instance, Windows ו-macOS

1. הפעל פעמיים על אותה תיקייה. ההפעלה השנייה מביאה את החלון הראשון לחזית ויוצאת. חזור כשהחלון מוסתר ב-tray.
2. סגור את המופע בכוח והפעל מחדש. `debug.log` מציג `stale owner record replaced`.
3. השהה את המופע (`kill -STOP`, או Resource Monitor ב-Windows) והפעל שני. אחרי כ-4 שניות מופיע דיאלוג עם pid וגרסה.
4. הגדר `YMUX_CONFIG_DIR` והרץ לצד release. כל hook חייב להגיע ל-build שלו. ב-macOS בדוק גם pane של tmux כששרת tmux כבר רץ.
5. התקן עדכון (NSIS, dmg). הגרסה החדשה עולה.
6. הרץ v0.5.0 לצד build חדש ושמור שלוש פעמים בכל אחד. כל ה-workspaces נשארים.

## #93 daemon 2.20.0, דפדפן

1. ב-desktop, ודא ששרת 2.19.0 מקבל הצעה ל-2.20.0, והתקן.
2. בסשן דפדפן תחת `none`, בקש מ-Claude להריץ `mkfs --version`. התוצאה: deny וכרטיס `⛔ Blocked: Bash`, בלי כרטיס אישור.
3. חזור תחת `gate`. התוצאה: deny בלי המתנה.
4. בסשן Claude בדפדפן, כתוב brief עם `goal:` והרץ `/compact`. התוצאה: `[ymux-context]` עם Goal, Now ו-Recent progress.
5. כבה "Inject context into sessions" והרץ `/compact`. התוצאה: בלי context.

## #90 `tauri` 2.11.6 ו-`zip` 9.5, Windows ו-macOS

1. הפעל את האפליקציה. החלון הראשי עולה וטרמינל עובד.
2. Sidebar 🌐: פתח Browser על port מועבר. הדף מוצג.
3. פתח modal: ה-webview מוסתר. סגור: הוא חוזר.
4. נווט ל-URL שני באותו webview.
5. סגור ופתח שוב את ה-Browser. ב-debug log אין שגיאת `add_child`.
6. Dev Mode: לכידת אלמנט עדיין יוצרת ticket.
7. הוצא את ה-Browser לחלון נפרד.
8. File Manager: zip ו-unzip מקומי. התקנת פונט מ-zip.

## #89 smart-bidi

1. `settings.json` ישן בלי `smart_bidi`: התיבה ב-Settings → Terminal → RTL כבויה ב-local וב-remote.
2. עברית עם `./x --flag`, ב-`auto_per_line` וב-`force_rtl`, עם התיבה דלוקה וכבויה, אחרי reconnect.
3. Claude Code תחת tmux ו-zellij עם התיבה דלוקה: אין הזזת שורות ואין `B` תועה.
4. פונט שבו רוחב העברית שונה מהלטינית.
5. העתק token עטוף והדבק ב-shell: אין תו נסתר.
6. pane של Claude מקומי (visual order).
7. leaf עם `smart_bidi: true` שמור ופרופיל כבוי: אין isolates אחרי חיבור.
8. הפעל מחדש עם התיבה דלוקה: panes משוחזרים מסוננים בלי reconnect.

## #85 פרוטוקול brief ב-SessionStart

דורש build ש-CLI השרת שלו מכיל `context.inject`.

1. פתח סשן Claude חדש ב-pane של ymux: התשובה הסופית מסתיימת ב-`[ymux-brief]`.
2. הרץ `/clear`: אותו דבר.
3. כבה את ה-toggle: אין בלוק בסשן הבא.
4. פתח את ה-empty state של ה-Queue: ה-snippet מוצג נכון.

## #84 port-watch ו-hooks

דורש release שמגיע ל-CLI בשרת.

1. בסשן Claude בשרת, ודא ב-`~/.ymux/hook-debug.log` את `session-start outcome=injected` ב-resume או compact.
2. קרא את שדות הזמן. `connect_ms` אמור להיות קרוב ל-0.
3. פתח port חדש. ה-desktop מזהה אותו תוך 3 שניות.

## #88 BRIEF מול overview

1. פתח Active sessions ב-workspace עם pane של סוכן: השורה מציגה סטטוס Queue מיד, עם checkbox כבוי.
2. שלח prompt: אחרי פתיחה מחדש השורה מציגה working.
3. סשן בלי pane: ניתן לסימון, והתווית היא "read from screen".
4. שני hosts עם סשן `dev` ו-pane id ישן באחד: השורה חוזרת ל-LLM.
5. בדוק את התווית בעברית.
6. פתח בדפדפן: השורות משתמשות ב-LLM.

## #95 הערות diff

1. pane מקומי של Windows ב-zellij עם Claude Code: הדבק 2 הערות. ודא קלט אחד מרובה שורות ושדבר לא נשלח. חזור ב-PowerShell רגיל, עם תוכן לא מזיק.
2. SSH עם Claude Code ב-tmux: אותה בדיקה.
3. UI בעברית: כפתור ההוספה והגאטר משמאל, טקסט ההערה RTL.
4. דפדפן: כפתור ההדבקה מוצג ועובד.
5. pane שהוצא לחלון נפרד: הטקסט מגיע אליו.

## #96 קובץ הגדרות worktree

1. SSH: בצע commit למתכון עם `.env` ו-`"setup": "echo ok"`, צור worktree, וודא שה-prompt מציג את הרשימה והפקודה. אשר, וודא העתקה ו-setup. worktree שני: בלי prompt.
2. שנה את המתכון: prompt מופיע שוב.
3. הפוך את `.env` ל-symlink: המתכון נעצר לפני setup.
4. setup עם `exit 3`: ה-worktree נשאר, ויוצגו קוד היציאה והפלט.
5. macOS מקומי: `npm` נמצא דרך `/bin/sh -lc`.
6. Windows מקומי: קבצים מועתקים, ה-prompt מציג את ההערה, ו-setup לא רץ.

## #97 פקודות CLI

1. Windows: `ymux read` על pane מקומי. ודא ש-`dump-screen --full` כותב ל-stdout.
2. macOS ו-SSH: `ymux read` על pane עם persistence.
3. pane עם hooks: `send`, אחר כך `wait --for idle --after-seq`, אחר כך `read`.
4. pane בלי hooks: יציאה בקוד 5.
5. SSH: `ymux worktree create`.

## #94 jump palette (תלוי ב-#87), Windows ו-macOS

1. לחץ `Ctrl+Shift+O` כשטרמינל ממוקד: ה-palette נפתח ושום דבר לא מגיע ל-shell.
2. pane חסום ב-workspace אחר הוא השורה הראשונה. Enter מחליף workspace וממקד את ה-pane.
3. עברית: כיוון שורה ושאילתה מעורבת עם path.
4. קורא מסך: החצים מקריאים את הסטטוס.
5. שנה קיצור ב-Settings: הרמז ב-command palette משתנה.

## #87 command palette

1. ב-UI בעברית הקלד מילה באנגלית: הפקודה מופיעה.
2. הקלד `close pane`: הפקודה מופיעה.
3. שנה קיצור של Close Pane: הרמז מתעדכן בלי הפעלה מחדש.
4. בעברית הרמז משמאל ונקרא `Ctrl+Shift+W` משמאל לימין.
5. חצים, Enter ו-Escape עובדים כמו קודם.
6. תווית ארוכה נקטעת והרמז נשאר גלוי.

## #86 מעצבי גודל ו-dead code

1. Settings, כפתור התקנת פונט: בדוק את תווית הגודל.
2. File Manager, פתיחת קובץ בעורך: בדוק את תווית הגודל.
3. Monitor → Analytics, rx/tx ללא תעבורה: `0 B/s`.
4. פאנל Browser: שנה גודל חלון. ה-webview עוקב. ב-Windows וב-macOS.
