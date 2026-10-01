# restart-motd

Winziges Tool, das vor dem Minecraft-Server sitzt. Es macht nur eins:
Solange der Server neu startet (z. B. nachts durch Simple Auto Restart), zeigt
die Serverliste die Standard-MOTD mit der roten Zeile **"Der Server wird neu
gestartet"**. Wer in der Zeit joinen will, bekommt eine freundliche Kick-Nachricht.

Ist der Server oben, wird alles unveraendert durchgereicht (der Server zeigt
seine normale MOTD). Das Tool startet oder stoppt den Server **nie**.

## Einrichtung

1. In `server.properties` den echten Server auf einen anderen Port legen, z. B. `server-port=25566`.
2. Bauen: `cargo build --release`
3. `restart-motd.example.toml` nach `restart-motd.toml` kopieren und anpassen
   (`listen` = oeffentlicher Port 25565, `server` = Adresse des echten Servers,
   MOTD-Texte). Ohne Datei gelten die Standardwerte.
4. Starten: `./restart-motd` (optional mit Pfad zur Config als Argument).

Die echte Spieler-IP wird standardmaessig per PROXY-Protocol v2 an den Server
weitergegeben (`proxy_protocol = true`). Dafuer muss der Server es aktiviert
haben (z. B. Paper: `proxy-protocol: true` in `paper-global.yml`; bei
Velocity/Bungee entsprechend). Ohne Unterstuetzung auf dem Server mit
`proxy_protocol = false` abschalten.
