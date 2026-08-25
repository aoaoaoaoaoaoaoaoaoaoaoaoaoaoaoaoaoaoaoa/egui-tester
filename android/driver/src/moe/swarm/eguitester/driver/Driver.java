package moe.swarm.eguitester.driver;

import android.app.Activity;
import android.app.Instrumentation;
import android.app.UiAutomation;
import android.os.Bundle;
import android.os.SystemClock;
import android.os.Trace;
import android.view.InputDevice;
import android.view.KeyCharacterMap;
import android.view.KeyEvent;
import android.view.MotionEvent;

public final class Driver extends Instrumentation {
    private Bundle arguments;

    @Override
    public void onCreate(Bundle arguments) {
        this.arguments = arguments;
        start();
    }

    @Override
    public void onStart() {
        Bundle result = new Bundle();
        try {
            String gesture = required("gesture");
            Trace.beginSection("egui-tester: " + required("action"));
            try {
                switch (gesture) {
                    case "tap":
                        tap(integer("x"), integer("y"));
                        break;
                    case "swipe":
                        swipe(
                            integer("x0"), integer("y0"),
                            integer("x1"), integer("y1"),
                            integer("steps"), integer("duration_ms")
                        );
                        break;
                    case "hold":
                        hold(
                            integer("x"), integer("y"),
                            integer("steps"), integer("duration_ms")
                        );
                        break;
                    case "text":
                        text(required("text"));
                        break;
                    case "key":
                        key(required("key"));
                        break;
                    case "pinch":
                        pinch(
                            integer("x0"), integer("y0"),
                            integer("x1"), integer("y1"),
                            integer("dx"), integer("dy"),
                            integer("steps"), integer("duration_ms")
                        );
                        break;
                    default:
                        throw new IllegalArgumentException(
                            "unknown -e gesture " + gesture
                        );
                }
            } finally {
                Trace.endSection();
            }
            result.putString("stream", gesture + " injected\n");
            finish(Activity.RESULT_OK, result);
        } catch (RuntimeException failure) {
            result.putString("stream", failure.toString() + "\n");
            finish(Activity.RESULT_CANCELED, result);
        }
    }

    private int integer(String name) {
        return Integer.parseInt(required(name));
    }

    private String required(String name) {
        String value = arguments.getString(name);
        if (value == null) {
            throw new IllegalArgumentException("missing -e " + name);
        }
        return value;
    }

    private void tap(int x, int y) {
        UiAutomation automation = getUiAutomation();
        long downTime = SystemClock.uptimeMillis();
        inject(
            automation, downTime, downTime, MotionEvent.ACTION_DOWN,
            x, y, x, y, 1
        );
        inject(
            automation, downTime, downTime + 16, MotionEvent.ACTION_UP,
            x, y, x, y, 1
        );
    }

    private void swipe(
        int x0, int y0, int x1, int y1, int steps, int durationMs
    ) {
        requireMotion(steps, durationMs);
        UiAutomation automation = getUiAutomation();
        long downTime = SystemClock.uptimeMillis();
        inject(
            automation, downTime, downTime, MotionEvent.ACTION_DOWN,
            x0, y0, x0, y0, 1
        );
        for (int step = 1; step <= steps; step++) {
            long eventTime = downTime + (long) durationMs * step / steps;
            float phase = (float) step / steps;
            float x = x0 + (x1 - x0) * phase;
            float y = y0 + (y1 - y0) * phase;
            inject(
                automation, downTime, eventTime, MotionEvent.ACTION_MOVE,
                x, y, x, y, 1
            );
            SystemClock.sleep(Math.max(1L, durationMs / steps));
        }
        inject(
            automation, downTime, downTime + durationMs,
            MotionEvent.ACTION_UP, x1, y1, x1, y1, 1
        );
    }

    private void hold(int x, int y, int steps, int durationMs) {
        swipe(x, y, x, y, steps, durationMs);
    }

    private void text(String text) {
        KeyEvent[] events = KeyCharacterMap
            .load(KeyCharacterMap.VIRTUAL_KEYBOARD)
            .getEvents(text.toCharArray());
        if (events == null) {
            throw new IllegalArgumentException(
                "Android cannot forge text " + text
            );
        }
        UiAutomation automation = getUiAutomation();
        for (KeyEvent event : events) {
            if (!automation.injectInputEvent(event, true)) {
                throw new IllegalStateException(
                    "Android rejected injected KeyEvent"
                );
            }
        }
    }

    private void key(String name) {
        int code = KeyEvent.keyCodeFromString(name);
        if (code == KeyEvent.KEYCODE_UNKNOWN) {
            throw new IllegalArgumentException("unknown Android key " + name);
        }
        UiAutomation automation = getUiAutomation();
        long downTime = SystemClock.uptimeMillis();
        injectKey(automation, downTime, downTime, KeyEvent.ACTION_DOWN, code);
        injectKey(
            automation, downTime, downTime + 16, KeyEvent.ACTION_UP, code
        );
    }

    private void pinch(
        int x0, int y0, int x1, int y1,
        int dx, int dy, int steps, int durationMs
    ) {
        requireMotion(steps, durationMs);
        UiAutomation automation = getUiAutomation();
        long downTime = SystemClock.uptimeMillis();
        inject(automation, downTime, downTime, MotionEvent.ACTION_DOWN,
            x0, y0, x1, y1, 1);
        inject(
            automation,
            downTime,
            downTime + 8,
            MotionEvent.ACTION_POINTER_DOWN
                | (1 << MotionEvent.ACTION_POINTER_INDEX_SHIFT),
            x0, y0, x1, y1, 2
        );
        for (int step = 1; step <= steps; step++) {
            long eventTime = downTime + 8L + (long) durationMs * step / steps;
            float phase = (float) step / steps;
            inject(
                automation, downTime, eventTime, MotionEvent.ACTION_MOVE,
                x0 - dx * phase, y0 - dy * phase,
                x1 + dx * phase, y1 + dy * phase, 2
            );
            SystemClock.sleep(Math.max(1L, durationMs / steps));
        }
        long lifted = downTime + 8L + durationMs;
        inject(
            automation,
            downTime,
            lifted,
            MotionEvent.ACTION_POINTER_UP
                | (1 << MotionEvent.ACTION_POINTER_INDEX_SHIFT),
            x0 - dx, y0 - dy, x1 + dx, y1 + dy, 2
        );
        inject(
            automation, downTime, lifted + 8, MotionEvent.ACTION_UP,
            x0 - dx, y0 - dy, x1 + dx, y1 + dy, 1
        );
    }

    private static void requireMotion(int steps, int durationMs) {
        if (steps < 1 || durationMs < 1) {
            throw new IllegalArgumentException(
                "steps and duration_ms must be positive"
            );
        }
    }

    private static void inject(
        UiAutomation automation,
        long downTime,
        long eventTime,
        int action,
        float x0,
        float y0,
        float x1,
        float y1,
        int pointers
    ) {
        MotionEvent.PointerProperties[] properties =
            new MotionEvent.PointerProperties[pointers];
        MotionEvent.PointerCoords[] coordinates =
            new MotionEvent.PointerCoords[pointers];
        for (int index = 0; index < pointers; index++) {
            MotionEvent.PointerProperties property =
                new MotionEvent.PointerProperties();
            property.id = index;
            property.toolType = MotionEvent.TOOL_TYPE_FINGER;
            properties[index] = property;

            MotionEvent.PointerCoords coordinate = new MotionEvent.PointerCoords();
            coordinate.x = index == 0 ? x0 : x1;
            coordinate.y = index == 0 ? y0 : y1;
            coordinate.pressure = 1.0f;
            coordinate.size = 1.0f;
            coordinates[index] = coordinate;
        }
        MotionEvent event = MotionEvent.obtain(
            downTime, eventTime, action, pointers, properties, coordinates,
            0, 0, 1.0f, 1.0f, 0, 0, InputDevice.SOURCE_TOUCHSCREEN, 0
        );
        try {
            if (!automation.injectInputEvent(event, true)) {
                throw new IllegalStateException(
                    "Android rejected injected MotionEvent"
                );
            }
        } finally {
            event.recycle();
        }
    }

    private static void injectKey(
        UiAutomation automation,
        long downTime,
        long eventTime,
        int action,
        int code
    ) {
        KeyEvent event = new KeyEvent(
            downTime,
            eventTime,
            action,
            code,
            0,
            0,
            KeyCharacterMap.VIRTUAL_KEYBOARD,
            0,
            0,
            InputDevice.SOURCE_KEYBOARD
        );
        if (!automation.injectInputEvent(event, true)) {
            throw new IllegalStateException(
                "Android rejected injected KeyEvent"
            );
        }
    }
}
