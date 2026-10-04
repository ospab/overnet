plugins {
    id("com.android.application") version "8.13.2" apply false
    // GeckoView is built with Kotlin 2.4: an older compiler can't read it.
    id("org.jetbrains.kotlin.android") version "2.4.20" apply false
}
