import i18n from 'i18next';
import { initReactI18next } from 'react-i18next';
import en from './locales/en.json';
import zh from './locales/zh.json';
import zhTW from './locales/zh-TW.json';

const resources = {
  en: { translation: en },
  zh: { translation: zh },
  'zh-TW': { translation: zhTW },
};

// Get saved language or detect from system
const getSavedLanguage = () => {
  if (typeof window !== 'undefined') {
    const saved = localStorage.getItem('language');
    if (saved && ['en', 'zh', 'zh-TW'].includes(saved)) {
      return saved;
    }
    // Detect from system
    const systemLang = navigator.language;
    if (systemLang.startsWith('zh')) {
      if (systemLang === 'zh-TW' || systemLang === 'zh-HK') {
        return 'zh-TW';
      }
      return 'zh';
    }
  }
  return 'en';
};

i18n
  .use(initReactI18next)
  .init({
    resources,
    lng: getSavedLanguage(),
    fallbackLng: 'en',
    interpolation: {
      escapeValue: false,
    },
  });

export default i18n;
