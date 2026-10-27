const { contextBridge, ipcRenderer } = require('electron');

contextBridge.exposeInMainWorld('native', {
    invoke: (cmd, args) => ipcRenderer.invoke('native:' + cmd, args || {})
});
