# Диагностика

| Симптом | Что проверить |
|---|---|
| Изменение в feature branch не применяется | Это ожидаемо: нужен approved merge в `repository.branch` |
| `waiting_for_players` | Есть игроки или они переподключились в grace period; дождитесь пустого сервера |
| RCON недоступен | Existing secret env, port, running service, firewall; не обходите проверку как «0 игроков» |
| Hash mismatch | Точный релиз/байты, source provenance, Git pin; не заменяйте hash вслепую |
| Transfer pin без receipt | Выполнить независимый send→verify→commit или adopt import; один файл кеша не достаточен |
| `source changed since plan` | Файлы изменились, сделайте новый plan и review |
| `Arbitrary config needs explicit approval` | Проверьте файл и добавьте точный путь в export-policy.json через PR |
| `Potential secret` | Удалите секрет вручную из Git-конфига, используйте поддерживаемый локальный secret механизм; не добавляйте bypass |
| `Forbidden tracked file` | `.gitignore` не удаляет уже tracked файл; исключите runtime и расследуйте историю/секреты |
| PR отказал из-за HEAD | Clone должен быть на свежем deploy branch HEAD перед edits; актуализируйте/rebase и review заново |
| PR push прошел, gh create упал | Ветка сохранена; проверить gh permissions и открыть PR вручную |
| Adopt отказал | Unit/path/fingerprint, exact approved SHA, configs/JAR и existing RCON должны совпадать |
| Target world commit запрещен | Остановите target, verify все файлы и явно подтвердите world replacement |
| Transfer interrupted | Повторить send с тем же private session file, проверить offsets/status |
| `recovery_required` | Не стартовать старую Java автоматически; следовать [Recovery](Recovery.md) |
| Quota/free space/TTL | Проверить transfer limits, диск и срок сессии; не удалять active journal |
| Actions checkout private tool repo | Шаблон предполагает public source; нужен отдельный authorized cross-repo credential, не меняйте visibility автоматически |

Логи сервисов смотрите через `journalctl -u gitcraft-agent.service`, `journalctl -u gitcraft-web.service` и ваш Minecraft unit. Перед отправкой лога другим людям удалите секреты и персональные данные. GitCraft не отправляет логи обратно в config Git.

В рабочей среде разработки именованный Rust toolchain 1.95.0 однажды столкнулся с локальным конфликтом rustup. При уже установленном stable **той же версии** можно использовать `RUSTUP_TOOLCHAIN=stable cargo test --locked`; предварительно проверьте `rustc --version`. Это не обещание совместимости с любым старым stable и не причина менять глобальную среду на сервере.
