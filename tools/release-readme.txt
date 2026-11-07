stem-split
==========

Splits an audio file into stems: drums, bass, vocals and the rest.
Everything runs on this computer; no file is sent anywhere.

How to use
----------
1. Keep all the files of this folder together and run stem-split.exe.
2. Choose one or more audio files (MP3, FLAC, WAV, OGG or M4A), or drag them
   onto the window. Parallel tracks are configurable in Settings (default: 2).
3. Press "Split". The stems appear next to the file as
   <name>.drums.wav, <name>.bass.wav, <name>.other.wav and <name>.vocals.wav.
   Choose WAV or MP3 beside Split, or use Split and mix for selected stems.
4. Use the URL tab to inspect/download recordings or select playlist entries.
   Downloads can keep the original format or be saved as WAV/MP3.
5. Open Settings with the gear beside the language selector to manage yt-dlp
   and FFmpeg, change/open the models/cache folder, and set parallel tracks.

The first run with each model downloads it (110 to 675 MB) into
%LOCALAPPDATA%\stem-split\cache\models by default. The folder can be changed
in Settings. Cached models allow offline processing of local audio.

The interface language is changed with the globe beside the settings gear.
The small buttons next to the model list open the folder with the downloaded
models and delete them to free the space.

Requirements
------------
Windows 10 or 11, 64-bit. A graphics card with DirectX 12 makes the work
about ten times faster; without one the processor is used.

If something goes wrong
-----------------------
The application writes warnings and errors to
%LOCALAPPDATA%\stem-split\data\stem-split.log, started afresh on every launch.
If the graphics card misbehaves, choose "Processor" in the "Device" list.

Third-party components
----------------------
ONNX Runtime (Microsoft, MIT licence); DirectML and the Visual C++ runtime
libraries (Microsoft, redistributed under their own licence terms); the
HTDemucs models (Meta; the code is under the MIT licence, the weights may be
limited to non-commercial use). URL downloads use yt-dlp and FFmpeg with
their own licensing terms; the Rust yt-dlp dependency is GPL-3.0-only.
