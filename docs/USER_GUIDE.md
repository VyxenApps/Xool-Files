# Xool Files user guide

Created by **Vyxen Apps**. [VyxenApps on GitHub](https://github.com/VyxenApps)

## Navigation and files

- Use the address bar to navigate. While editing a path, press **↑** or **↓** to move through recent paths and **Enter** to open the highlighted one.
- Middle-click a folder to open it in a tab. Middle-click a tab to close it. Closing the last tab follows the same settings as clicking the window’s **X**.
- Click **Split** to open another pane, or drag a tab to a pane edge. To split a pane by dragging one of its tabs, at least one other tab must remain in the original pane.
- Change the sort order and view from **Sort and View** or the status bar. Under **General → Layout**, you can move the status bar below the tabs or to the bottom of the window.
- Go to **Settings → Files and menus** to change how files open, set confirmation for opening multiple items, and reorder the context menu shortcuts.
- **Ctrl+Z** can undo rename, supported move, and Recycle Bin operations when Xool Files can identify the original item and the destination is available.

## Search

- **Search in this folder** filters the current view. **SUB** includes subfolders.
- You can combine terms and filters. For example, `report ext:pdf,txt -draft` finds PDF or TXT reports and excludes names containing “draft.” Use `~report` for fuzzy matching. The file-type filter uses the literal prefix `tipo:` followed by the type label.
- Go to **Settings → Search** to configure the global shortcut, try the search tool, and refresh the system index. The index stores file names and paths.

## Preview and Recycle Bin

- **Preview** shows information about the selected file and displays its contents for supported formats.
- To edit text, enable **Files and menus → Preview → Allow editing text files**. Click **Edit** to edit UTF-8 TXT, MD, LOG, JSON, and CSV files up to 1 MB. Save with **Save** or **Ctrl+S**.
- Open **Recycle Bin** from Quick Access. Select an item and press **Enter** or use its context menu to restore it. To empty the bin, use the context menu on an empty area or on the Recycle Bin entry in the sidebar.

## Settings and local data

| Data | Location |
| --- | --- |
| Preferences, tabs, and favorites | `%APPDATA%\XoolFiles\settings.json` |
| Thumbnail cache | `%LOCALAPPDATA%\XoolFiles\cache` |
| Search index | `%LOCALAPPDATA%\XoolFiles\search-index-v1.txt` |
| Error log, if generated | Next to `settings.json` |

Go to **Settings → About → Settings file** and click **Open folder** to open the configuration folder in Xool Files. From the same section, you can import, export, or reset your settings. Importing or resetting saves a backup of your previous settings.

Use **Import settings** to restore preferences from a compatible backup.
