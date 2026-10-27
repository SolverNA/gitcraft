# Ветки и Pull Request

## Граница релиза

Агент следит только за `repository.branch` и фиксирует точный SHA в начале подготовки. Push разработчика в свою ветку ничего не разворачивает. Релиз появляется только после merge в настроенную deploy-ветку. GitHub branch protection и required reviews/checks настраиваются администратором; presence CODEOWNERS само по себе не включает обязательность review.

Для редактирования используйте отдельный обычный working clone. Bare repository агента и его staging не являются редактором конфигов. Ветку `main` можно заменить на `release`, но GitHub base и agent config должны совпадать.

## Предложить изменение CLI

Начните от свежего HEAD deploy-ветки. Измените конфиг локально, посмотрите diff и выполните:

```sh
git -C /home/operator/minecraft-config diff
gitcraft validate --repository /home/operator/minecraft-config
gitcraft propose --repository /home/operator/minecraft-config \
  --base release --title 'Tune view distance'
```

Нужна локальная авторизация `gh` с правом создавать ветку/PR. Это авторизация разработчика, не read-only deploy key машины.

Команда проверяет общий policy, получает branch HEAD, требует, чтобы текущий HEAD рабочего clone совпадал с ним, создает уникальную `gitcraft/config-*` ветку, атомарный commit изменения, push этой ветки и PR. В PR фиксируются base/proposed SHA. Прямого push в deploy-ветку нет. Если base уже изменился — актуализируйте clone и проверьте diff заново.

Разрешены изменения approved конфигов, точных `requirements/*.json` и `export-policy.json`. Смена workflows/кода и произвольных файлов не входит в этот конфигурационный PR command: используйте отдельный явно проверяемый development процесс. Панель не должна превращать браузерный input в произвольный shell или файловую систему.

Если push прошел, а создание PR не удалось из-за прав или сети, ветка остается на GitHub. Откройте PR вручную или исправьте локальную авторизацию; программа не удаляет ветку и не делает force-push.

## Бот зависимостей

Workflow обновлений лежит в `templates/config-repository/.github/workflows/` и включается только после настройки config-репозитория. В репозитории инструмента это файл шаблона, а не активный updater.

Для Modrinth бот выбирает `release` для точных Minecraft/loader, проверяет provider SHA-512/размер, записывает SHA-256 и открывает PR. Он не меняет Minecraft/loader самостоятельно и не угадывает неизвестные источники. Transfer/direct без поддерживаемого API не обновляются автоматически. Наличие новой стабильной версии не гарантирует совместимость плагинов друг с другом — это часть review.
