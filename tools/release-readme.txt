stem-split
==========

Splits an audio file into stems: drums, bass, vocals and the rest.
Everything runs on this computer; no file is sent anywhere.

How to use
----------
1. Keep all the files of this folder together and run stem-split.exe.
2. Choose one or more audio files (MP3, FLAC, WAV, OGG or M4A), or drag them
   onto the window. Several files are split two at a time.
3. Press "Split". The stems appear next to the file as
   <name>.drums.wav, <name>.bass.wav, <name>.other.wav and <name>.vocals.wav.

The first run with each model downloads it (110 to 675 MB) into
%LOCALAPPDATA%\stem-split\cache\models. After that no network is needed.

The interface language is changed with the globe button in the bottom right
corner of the main screen. The small buttons next to the model list open the
folder with the downloaded models and delete them to free the space.

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
limited to non-commercial use).
