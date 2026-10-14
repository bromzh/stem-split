stem-split — macOS / Apple Silicon
=================================

ENGLISH

Requirements: Apple Silicon Mac (M1 or newer), macOS 12 or newer.
This is not an Intel or universal build. Rust, Python and ffmpeg are not needed.
The application contains one executable with ONNX Runtime linked into it;
only macOS system frameworks are loaded. Models are downloaded on first use.
Audio is processed locally. CoreML can use the GPU and Neural Engine; if the
provider fails, processing falls back to the CPU. Acceleration varies by model.

Installation
1. Download the DMG from https://github.com/bromzh/stem-split/releases.
2. Open the DMG and drag stem-split.app onto the Applications shortcut.
3. Eject the disk image and open stem-split from Applications.

Gatekeeper / first launch
This build is ad-hoc signed, NOT signed with an Apple Developer ID and NOT
notarized by Apple. macOS may therefore block a downloaded copy.
Only allow this application if you trust its source.

Try opening the installed app once. If macOS blocks it:
- On macOS 13 or newer, open System Settings > Privacy & Security.
- On macOS 12, open System Preferences > Security & Privacy > General.
Find the message about stem-split, click "Open Anyway", then confirm "Open".
You may need to authenticate. On older macOS versions you can also try
Control-clicking the app in Finder and choosing Open, then Open again.

If macOS reports that the app is "damaged", first download it again from the
official release. To check the download against the accompanying .sha256 file,
place both files in the same folder, open Terminal there and run:
  shasum -a 256 -c stem-split-@VERSION@-macos-arm64.dmg.sha256
If you trust the download and it is still blocked, remove quarantine ONLY
from this installed application in Terminal (no sudo is needed):
  xattr -dr com.apple.quarantine "/Applications/stem-split.app"
Then open the app again. If you installed it elsewhere, adjust this path.
Do not disable Gatekeeper globally. Managed Macs may require administrator
approval; these steps do not override your organization's security policy.

Models: ~/Library/Caches/stem-split/models
Log: ~/Library/Application Support/stem-split/stem-split.log
Use the app's folder button to locate downloaded models.
Code: MIT (see LICENSE.txt). Model weights have separate licensing terms;
this application is intended for personal, non-commercial use.

РУССКИЙ

Требования: Mac с Apple Silicon (M1 или новее), macOS 12 или новее.
Это не сборка для Intel и не universal. Rust, Python и ffmpeg не нужны.
В приложении один исполняемый файл со встроенным ONNX Runtime; внешние
библиотеки — только системные фреймворки macOS. Модели скачиваются при первом
использовании. Аудио обрабатывается локально. CoreML может использовать GPU
и Neural Engine; при отказе провайдера обработка переходит на CPU. Скорость
ускорения зависит от модели.

Установка
1. Скачайте DMG с https://github.com/bromzh/stem-split/releases.
2. Откройте DMG и перетащите stem-split.app на ярлык Applications.
3. Извлеките образ диска и откройте stem-split из «Программы».

Gatekeeper / первый запуск
Сборка подписана локально (ad-hoc), но НЕ сертификатом Apple Developer ID
и НЕ прошла нотариализацию Apple. Поэтому macOS может блокировать скачанное
приложение. Разрешайте запуск только если доверяете источнику.

Попробуйте открыть установленное приложение один раз. Если macOS блокирует:
- В macOS 13 и новее: «Системные настройки» > «Конфиденциальность и безопасность».
- В macOS 12: «Системные настройки» > «Защита и безопасность» > «Основные».
Найдите сообщение о stem-split, нажмите «Всё равно открыть» («Open Anyway»),
затем подтвердите «Открыть». Может потребоваться пароль. В старых версиях
macOS также можно нажать Control и щёлкнуть приложение в Finder, выбрать
«Открыть» и ещё раз подтвердить «Открыть».

Если macOS сообщает, что приложение «повреждено», сначала скачайте его заново
из официального релиза. Для сверки с файлом .sha256 положите оба файла в одну
папку, откройте в ней Терминал и выполните:
  shasum -a 256 -c stem-split-0.1.0-macos-arm64.dmg.sha256
Если доверяете загрузке, но блокировка остаётся, снимите quarantine ТОЛЬКО
с установленного приложения в Терминале (sudo не нужен):
  xattr -dr com.apple.quarantine "/Applications/stem-split.app"
После этого откройте приложение снова. Если установили в другую папку,
измените путь. Не отключайте Gatekeeper целиком. На корпоративном Mac может
потребоваться разрешение администратора; инструкция не обходит политику
безопасности организации.

Модели: ~/Library/Caches/stem-split/models
Журнал: ~/Library/Application Support/stem-split/stem-split.log
Папку моделей можно открыть кнопкой в приложении.
Код: MIT (см. LICENSE.txt). У весов моделей отдельные условия лицензирования;
приложение рассчитано на личное некоммерческое использование.
