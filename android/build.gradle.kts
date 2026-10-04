plugins {
    id("com.android.application") version "9.4.1" apply false
    // AGP 9 compiles Kotlin itself; this only picks the Kotlin version:
    // GeckoView is built with Kotlin 2.4 and an older compiler can't read it.
    id("org.jetbrains.kotlin.android") version "2.4.20" apply false
}
