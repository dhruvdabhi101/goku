const { contextBridge, ipcRenderer } = require('electron');
contextBridge.exposeInMainWorld('goku', {
  permissions: () => ipcRenderer.invoke('sdk', 'permissions'),
  requestPermission: kind => ipcRenderer.invoke('sdk', 'requestPermission', kind),
  openSettings: kind => ipcRenderer.invoke('sdk', 'openSettings', kind),
  start: () => ipcRenderer.invoke('sdk', 'start'),
  stop: () => ipcRenderer.invoke('sdk', 'stop'),
  pause: () => ipcRenderer.invoke('sdk', 'pause'),
  resume: () => ipcRenderer.invoke('sdk', 'resume'),
  captureInFive: () => ipcRenderer.invoke('capture-in-five'),
  status: () => ipcRenderer.invoke('sdk', 'status'),
  events: () => ipcRenderer.invoke('sdk', 'events'),
  recentFrames: () => ipcRenderer.invoke('sdk', 'recentFrames'),
  frame: id => ipcRenderer.invoke('sdk', 'frame', id),
  frameImage: id => ipcRenderer.invoke('sdk', 'frameImage', id),
  openData: () => ipcRenderer.invoke('open-data'),
  relaunch: () => ipcRenderer.invoke('relaunch'),
  onCapture: callback => ipcRenderer.on('capture-result', (_event, result) => callback(result)),
});
