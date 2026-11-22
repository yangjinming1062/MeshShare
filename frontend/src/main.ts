// Android 10 的系统 WebView 缺少 Svelte 渲染模板所需的 replaceAll。
import 'core-js/es/string/replace-all';
import App from './App.svelte';
import './app.css';
import { mount } from 'svelte';

const app = mount(App, { target: document.getElementById('app')! });

export default app;
