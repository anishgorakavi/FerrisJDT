package app;

import java.util.*;
import java.util.function.*;
import java.util.stream.*;

public class Shapes {
    sealed interface Shape permits Circle, Rect, Tri {}
    record Circle(double radius) implements Shape {}
    record Rect(double w, double h) implements Shape {
        Rect {
            if (w < 0) throw new IllegalArgumentException();
        }
        double area() { return w * h; }
        static Rect square(double s) { return new Rect(s, s); }
    }
    record Tri(double b, double h) implements Shape {}
    record Pair<A, B>(A first, B second) {}

    enum Level { LOW, HIGH; Level flip() { return this == LOW ? HIGH : LOW; } }

    static double area(Shape s) {
        return switch (s) {
            case Circle c -> Math.PI * c.radius() * c.radius();
            case Rect r -> r.area();
            case Tri t -> t.b() * t.h() / 2;
        };
    }

    static String describe(Object o) {
        if (o instanceof String str && str.length() > 3) {
            return str.toUpperCase();
        }
        return switch (o) {
            case Integer i when i > 10 -> "big " + i.intValue();
            case Integer i -> "int";
            case null -> "null";
            default -> "other";
        };
    }

    void run() {
        var list = new ArrayList<Pair<String, Integer>>();
        list.add(new Pair<>("a", 1));
        for (var p : list) {
            String f = p.first();
            int s = p.second();
            System.out.println(f.length() + s);
        }
        var names = list.stream().map(Pair::first).collect(Collectors.toList());
        names.get(0).trim();
        Rect r = Rect.square(2);
        double w = r.w();
        Level l = Level.LOW.flip();
        Map<String, List<Integer>> groups = new HashMap<>();
        groups.computeIfAbsent("k", k -> new ArrayList<>()).add(1);
        Function<Integer, Function<Integer, Integer>> add = a -> b -> a + b;
        int sum = add.apply(1).apply(2);
        String text = """
            hello
            """;
        text.strip();
    }
}
